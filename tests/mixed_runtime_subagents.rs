use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct Harness {
    collab: PathBuf,
    root: PathBuf,
    home: PathBuf,
    path: String,
    log: PathBuf,
    sessions: Vec<String>,
    parent_pane: String,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self
            .collab_cmd()
            .args(["down"])
            .env_remove("TMUX_PANE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for session in &self.sessions {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", session])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.home);
    }
}

impl Harness {
    fn collab_cmd(&self) -> Command {
        let mut command = Command::new(&self.collab);
        command
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("PATH", &self.path)
            .env("COLLAB_FIXTURE_LOG", &self.log)
            .env("TMUX_PANE", &self.parent_pane)
            .env_remove("COLLAB_WORKER")
            .env_remove("TMUX");
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        self.json_as(None, None, args)
    }

    fn json_as(&self, pane: Option<&str>, worker: Option<&str>, args: &[&str]) -> Value {
        let mut command = self.collab_cmd();
        if let Some(pane) = pane {
            command.env("TMUX_PANE", pane);
        }
        if let Some(worker) = worker {
            command.env("COLLAB_WORKER", worker);
        }
        command.args(args);
        let output = command.output().expect("run collab");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "collab {args:?} failed\nstdout={stdout}\nstderr={stderr}\nlog={}",
            fs::read_to_string(&self.log).unwrap_or_default()
        );
        serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
            panic!("json from collab {args:?}: {error}\nstdout={stdout}\nstderr={stderr}")
        })
    }
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn tmux_new(name: &str, cwd: &Path) -> (String, String) {
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id} #{session_name}",
            "-s",
            name,
            "-c",
        ])
        .arg(cwd)
        .arg("sleep")
        .arg("3600")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("tmux new-session");
    assert!(
        output.status.success(),
        "tmux new-session failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let mut parts = text.split_whitespace();
    (
        parts.next().expect("pane").into(),
        parts.next().expect("session").into(),
    )
}

fn ascii_hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn pane_capture(pane: &str) -> String {
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-J", "-t", pane])
        .output()
        .expect("capture-pane");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn pane_command(pane: &str) -> String {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_current_command}",
        ])
        .output()
        .expect("pane command");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn wait_pane_command(pane: &str, expected: &str) {
    let started = Instant::now();
    loop {
        if pane_command(pane) == expected {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "pane {pane} command stayed {} waiting for {expected}",
            pane_command(pane)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn set_title(pane: &str, title: &str) {
    assert!(Command::new("tmux")
        .args(["select-pane", "-t", pane, "-T", title])
        .status()
        .unwrap()
        .success());
}

fn rx_chunks(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("RX:"))
        .collect()
}

fn wait_rx(pane: &str, needle_hex: &str) -> String {
    let started = Instant::now();
    loop {
        let text = pane_capture(pane);
        let hex = rx_chunks(&text).concat();
        if hex.contains(needle_hex) && hex.contains("0d") {
            return text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "pane {pane} never received {needle_hex} with Enter:\n{text}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn cursor_and_codex_subagents_launch_and_message_each_other() {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("tmux is required for mixed-runtime regression");
    let pid = std::process::id();
    let stamp = Instant::now().elapsed().as_nanos();
    let root = std::env::temp_dir().join(format!("cmix{pid}-{stamp}"));
    let home = std::env::temp_dir().join(format!("cmixhome{pid}-{stamp}"));
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(home.join(".appsdk")).unwrap();
    fs::write(
        home.join(".appsdk/config.toml"),
        "[notifications]\nmode = \"immediate\"\n[subagent]\nruntime = \"cursor\"\n",
    )
    .unwrap();
    let log = root.join("fixture.log");
    let rec = root.join("rec.js");
    fs::write(
        &rec,
        r#"process.stdout.write('\x1b[?2004h');
if (process.env.COLLAB_FAKE_TUI === 'cursor') {
  process.stdout.write('  Cursor Grok 4.6 High Fast · 54.5%\n  /tmp/project · main\n  Run Everything\n');
} else {
  process.stdout.write('CODEX-TUI\n');
}
process.stdin.setRawMode(true);
process.stdin.resume();
process.stdin.on('data', b => {
  process.stdout.write('RX:' + b.toString('hex') + '\n');
});
setInterval(() => {}, 1 << 30);
"#,
    )
    .unwrap();
    write_exec(
        &bin.join("agent"),
        &format!(
            r#"#!/bin/sh
printf 'agent %s\n' "$*" >> "${{COLLAB_FIXTURE_LOG:-/dev/null}}"
if [ "$1" = status ]; then
  printf '{{"loggedIn":true,"authMethod":"fixture"}}\n'
  exit 0
fi
export COLLAB_FAKE_TUI=cursor
exec node "{rec}"
"#,
            rec = rec.display()
        ),
    );
    write_exec(
        &bin.join("codex"),
        &format!(
            r#"#!/bin/sh
printf 'codex %s\n' "$*" >> "${{COLLAB_FIXTURE_LOG:-/dev/null}}"
if [ "$1" = exec ]; then
  out=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --output-last-message) shift; out=$1 ;;
    esac
    shift
  done
  printf OK > "$out"
  exit 0
fi
unset COLLAB_FAKE_TUI
exec node "{rec}"
"#,
            rec = rec.display()
        ),
    );
    let path = format!(
        "{}:/opt/homebrew/bin:/usr/bin:/bin:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .unwrap()
        .success());
    let parent_name = format!("cmix-parent-{pid}");
    let (parent_pane, _) = tmux_new(&parent_name, &root);
    let mut harness = Harness {
        collab: PathBuf::from(env!("CARGO_BIN_EXE_collab")),
        root,
        home,
        path,
        log,
        sessions: vec![parent_name],
        parent_pane,
    };
    let init = harness.json(&["init"]);
    assert_eq!(init["ok"], true, "{init}");
    let cursor = harness.json(&[
        "subagent",
        "start",
        "--id",
        "cursor-rt",
        "--runtime",
        "cursor",
    ]);
    assert_eq!(cursor["subagent"]["runtime"], "cursor", "{cursor}");
    assert_eq!(cursor["close_required"], false);
    assert_eq!(cursor["next_check"], "status");
    assert_eq!(cursor["progress"], "snapshot");
    assert_eq!(cursor["subagent"]["status"], "starting", "{cursor}");
    let cursor_peer = cursor["subagent"]["peer"].as_str().unwrap().to_owned();
    let cursor_pane = cursor["subagent"]["pane"].as_str().unwrap().to_owned();
    harness.sessions.push(cursor_peer.clone());
    wait_pane_command(&cursor_pane, "node");
    set_title(&cursor_pane, "⠋ cursor-rt");
    let codex = harness.json(&[
        "subagent",
        "start",
        "--id",
        "codex-rt",
        "--runtime",
        "codex",
    ]);
    assert_eq!(codex["subagent"]["runtime"], "codex", "{codex}");
    assert_eq!(codex["subagent"]["status"], "starting", "{codex}");
    let codex_peer = codex["subagent"]["peer"].as_str().unwrap().to_owned();
    let codex_pane = codex["subagent"]["pane"].as_str().unwrap().to_owned();
    harness.sessions.push(codex_peer.clone());
    wait_pane_command(&codex_pane, "node");
    set_title(&codex_pane, "⠋ codex-rt");
    let fixture_log = fs::read_to_string(&harness.log).unwrap_or_default();
    assert!(
        fixture_log
            .lines()
            .any(|line| line.starts_with("agent status --format json")),
        "cursor probe must use official status json\n{fixture_log}"
    );
    assert!(
        !fixture_log.contains("--print"),
        "cursor probe must not use Codex-style --print\n{fixture_log}"
    );
    assert!(
        fixture_log
            .lines()
            .any(|line| line.starts_with("codex exec")),
        "codex probe must use exec\n{fixture_log}"
    );
    let cursor_ready = harness.json_as(
        Some(&cursor_pane),
        Some(&cursor_peer),
        &["subagent", "ready", "cursor-rt"],
    );
    assert!(cursor_ready.get("subagent").is_some(), "{cursor_ready}");
    let codex_ready = harness.json_as(
        Some(&codex_pane),
        Some(&codex_peer),
        &["subagent", "ready", "codex-rt"],
    );
    assert!(codex_ready.get("subagent").is_some(), "{codex_ready}");
    let ping = harness.json_as(
        Some(&cursor_pane),
        Some(&cursor_peer),
        &[
            "sendmessage",
            "--to",
            &codex_peer,
            "--subject",
            "cursor-to-codex",
            "ping from cursor",
        ],
    );
    assert_eq!(ping["notification"], "sent", "{ping}");
    let pong = harness.json_as(
        Some(&codex_pane),
        Some(&codex_peer),
        &[
            "sendmessage",
            "--to",
            &cursor_peer,
            "--subject",
            "codex-to-cursor",
            "pong from codex",
        ],
    );
    assert_eq!(pong["notification"], "sent", "{pong}");
    let cursor_inbox = harness.json_as(Some(&cursor_pane), Some(&cursor_peer), &["inbox"]);
    let codex_inbox = harness.json_as(Some(&codex_pane), Some(&codex_peer), &["inbox"]);
    assert!(
        format!("{cursor_inbox}").contains("codex-to-cursor"),
        "{cursor_inbox}"
    );
    assert!(
        format!("{codex_inbox}").contains("cursor-to-codex"),
        "{codex_inbox}"
    );
    let codex_rx = wait_rx(&codex_pane, &ascii_hex("cursor-to-codex"));
    let cursor_rx = wait_rx(&cursor_pane, &ascii_hex("codex-to-cursor"));
    let codex_chunks = rx_chunks(&codex_rx);
    let cursor_chunks = rx_chunks(&cursor_rx);
    assert!(
        codex_chunks.concat().contains("0d"),
        "codex session must receive Enter with the paste: {codex_rx}"
    );
    assert!(
        cursor_chunks
            .iter()
            .any(|chunk| chunk.contains(&ascii_hex("codex-to-cursor")) && !chunk.contains("0d")),
        "cursor payload must not share a chunk with Enter: {cursor_rx}"
    );
    assert!(
        cursor_chunks.iter().any(|chunk| *chunk == "0d"),
        "cursor session must receive a later Enter: {cursor_rx}"
    );
    let snapshot = harness.json(&["subagent", "snapshot", "cursor-rt", "--lines", "20"]);
    assert!(snapshot.get("screen_tail").is_some(), "{snapshot}");
    let _ = harness.json(&["subagent", "close", "cursor-rt"]);
    let _ = harness.json(&["subagent", "close", "codex-rt"]);
}
