use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static WAKE_BUFFER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Absent,
    Unknown,
    Working,
    Waiting,
}

pub fn pane_alive(pane: &str) -> bool {
    if !pane.starts_with('%') {
        return false;
    }
    // `tmux display-message -t <pane>` exits 0 even when the pane does not
    // exist, so enumerate all panes and compare pane ids exactly.
    Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output()
        .ok()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|id| id.trim() == pane)
        })
        .unwrap_or(false)
}

pub fn probe_agent_state(pane: &str) -> AgentState {
    if !pane.starts_with('%') {
        return AgentState::Absent;
    }
    let Some((command, title, screen)) = pane_view(pane) else {
        return AgentState::Absent;
    };
    let first = agent_state_from(&command, &title, &screen);
    if first != AgentState::Waiting {
        return first;
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    let Some((command2, title2, screen2)) = pane_view(pane) else {
        return AgentState::Absent;
    };
    if agent_state_from(&command2, &title2, &screen2) == AgentState::Working
        || title2 != title
        || screen2 != screen
    {
        AgentState::Working
    } else {
        AgentState::Waiting
    }
}

pub fn pane_idle(pane: &str) -> bool {
    probe_agent_state(pane) == AgentState::Waiting
}

pub fn pane_accepts_notification(pane: &str) -> bool {
    matches!(
        probe_agent_state(pane),
        AgentState::Working | AgentState::Waiting
    )
}

fn pane_view(pane: &str) -> Option<(String, String, String)> {
    let identity = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_current_command}\t#{pane_title}",
        ])
        .output()
        .ok()?;
    if !identity.status.success() {
        return None;
    }
    let output = String::from_utf8_lossy(&identity.stdout);
    let (command, title) = output.trim().split_once('\t')?;
    let screen = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane, "-S", "-40"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    Some((command.to_string(), title.to_string(), screen))
}

fn has_spinner(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim_start()
            .chars()
            .next()
            .is_some_and(|first| ('\u{2800}'..='\u{28ff}').contains(&first))
    })
}

fn cursor_model_line(screen: &str) -> bool {
    screen.lines().any(|line| {
        let Some(rest) = line.trim().strip_prefix("Cursor ") else {
            return false;
        };
        let model = rest
            .split('·')
            .next()
            .unwrap_or(rest)
            .split_whitespace()
            .next()
            .unwrap_or("");
        !model.is_empty() && model != "Agent"
    })
}

fn cursor_workspace_line(screen: &str) -> bool {
    screen
        .lines()
        .any(|line| line.trim().starts_with('/') && line.contains(" · "))
}

fn cursor_tui_confidence(screen: &str) -> u8 {
    let mut hits = 0u8;
    if cursor_model_line(screen) {
        hits += 1;
    }
    if screen.contains("Run Everything") {
        hits += 1;
    }
    if cursor_workspace_line(screen) {
        hits += 1;
    }
    hits.saturating_mul(33)
}

fn in_cursor_tui(screen: &str) -> bool {
    cursor_tui_confidence(screen) > 50
}

fn in_agent(command: &str, title: &str, screen: &str) -> bool {
    matches!(command, "codex" | "claude" | "agy" | "dsh")
        || ((matches!(command, "node" | "agent" | "cursor-agent"))
            && (in_cursor_tui(screen) || has_spinner(title)))
}

fn agent_state_from(command: &str, title: &str, screen: &str) -> AgentState {
    if !in_agent(command, title, screen) {
        return AgentState::Unknown;
    }
    if has_spinner(title) || has_spinner(screen) {
        AgentState::Working
    } else {
        AgentState::Waiting
    }
}

/// Cursor CLI swallows Enter that shares a PTY read with a bracketed-paste
/// terminator. Codex needs the opposite: `paste-buffer -p` then `C-m` in the
/// same tmux queue, or the paste lands without a submit.
const SUBMIT_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);
static KNOCK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmitKind {
    BracketedPaste,
    Literal,
}

fn submit_kind(command: &str, screen: &str) -> SubmitKind {
    if in_cursor_tui(screen) || matches!(command, "agent" | "cursor-agent") {
        SubmitKind::Literal
    } else {
        SubmitKind::BracketedPaste
    }
}

fn paste_args<'a>(pane: &'a str, text: &'a str, buffer: &'a str) -> Vec<&'a str> {
    vec![
        "set-buffer",
        "-b",
        buffer,
        text,
        ";",
        "paste-buffer",
        "-p",
        "-d",
        "-b",
        buffer,
        "-t",
        pane,
    ]
}

fn paste_submit_args<'a>(pane: &'a str, text: &'a str, buffer: &'a str) -> Vec<&'a str> {
    let mut args = paste_args(pane, text, buffer);
    args.extend([";", "send-keys", "-t", pane, "C-m"]);
    args
}

fn literal_args<'a>(pane: &'a str, text: &'a str) -> Vec<&'a str> {
    vec!["send-keys", "-t", pane, "-l", "--", text]
}

fn submit_args(pane: &str) -> [&str; 4] {
    ["send-keys", "-t", pane, "C-m"]
}

fn tmux(args: &[&str], what: &str, pane: &str) -> anyhow::Result<()> {
    let sent = Command::new("tmux").args(args).status()?;
    if !sent.success() {
        anyhow::bail!("tmux {what} delivery failed for pane {pane}");
    }
    Ok(())
}

fn knock_kind(pane: &str, text: &str, kind: SubmitKind) -> anyhow::Result<()> {
    let _lock = KNOCK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match kind {
        SubmitKind::Literal => {
            tmux(&literal_args(pane, text), "literal", pane)?;
            std::thread::sleep(SUBMIT_SETTLE);
            tmux(&submit_args(pane), "submit", pane)
        }
        SubmitKind::BracketedPaste => {
            let sequence = WAKE_BUFFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let buffer = format!("collab-wake-{}-{sequence}", std::process::id());
            tmux(&paste_submit_args(pane, text, &buffer), "paste-submit", pane)
        }
    }
}

pub fn knock(pane: &str, text: &str) -> anyhow::Result<()> {
    if !pane_alive(pane) {
        anyhow::bail!("pane {} not alive", pane);
    }
    if !pane_accepts_notification(pane) {
        anyhow::bail!("pane {} has no known agent", pane);
    }
    let kind = pane_view(pane)
        .map(|(command, _, screen)| submit_kind(&command, &screen))
        .unwrap_or(SubmitKind::BracketedPaste);
    knock_kind(pane, text, kind)
}

#[cfg(test)]
mod tests {
    use super::{
        agent_state_from, literal_args, paste_args, paste_submit_args, submit_kind,
        AgentState, SubmitKind,
    };

    #[test]
    #[ignore = "requires tmux and node; uses a disposable session"]
    fn live_working_pane_receives_one_batch_with_enter() {
        use std::process::Command;
        let session = format!("collab-batch-test-{}", std::process::id());
        let directory = std::env::temp_dir().join(&session);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("rec.js"),
            r#"process.stdout.write('\x1b[?2004h');
process.stdin.setRawMode(true);
process.stdin.resume();
process.stdin.on('data', b => {
  process.stdout.write('RX:' + b.toString('hex') + '\n');
});
setInterval(() => {}, 1 << 30);
"#,
        )
        .unwrap();
        let output = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                &session,
                "-c",
            ])
            .arg(&directory)
            .arg("node rec.js")
            .output()
            .unwrap();
        assert!(output.status.success());
        let pane = String::from_utf8(output.stdout).unwrap().trim().to_string();
        struct Cleanup(String, std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", &self.0])
                    .status();
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session, directory);
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(Command::new("tmux")
            .args(["select-pane", "-t", &pane, "-T", "⠋ batch-test"])
            .status()
            .unwrap()
            .success());
        assert_eq!(super::probe_agent_state(&pane), AgentState::Working);
        super::knock(
            &pane,
            "COLLAB_NOTIFY one [first] | COLLAB_NOTIFY two [second]",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let capture = Command::new("tmux")
            .args(["capture-pane", "-p", "-J", "-t", &pane])
            .output()
            .unwrap();
        let text = String::from_utf8(capture.stdout).unwrap();
        let chunks: Vec<_> = text.lines().filter_map(|l| l.strip_prefix("RX:")).collect();
        let hex = chunks.join("");
        assert!(hex.contains("6f6e65205b66697273745d"), "{text}");
        assert!(hex.contains("74776f205b7365636f6e645d"), "{text}");
        assert!(
            hex.contains("0d"),
            "Codex-style paste-submit must include Enter: {text}"
        );
        super::knock_kind(
            &pane,
            "COLLAB_NOTIFY cursor [literal]",
            super::SubmitKind::Literal,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let capture = Command::new("tmux")
            .args(["capture-pane", "-p", "-J", "-t", &pane])
            .output()
            .unwrap();
        let text = String::from_utf8(capture.stdout).unwrap();
        let chunks: Vec<_> = text.lines().filter_map(|l| l.strip_prefix("RX:")).collect();
        assert!(
            chunks
                .iter()
                .any(|c| c.contains("6c69746572616c") && !c.contains("0d")),
            "cursor literal payload must not include Enter: {text}"
        );
        assert!(
            chunks.iter().any(|c| *c == "0d"),
            "cursor submit is a later Enter of its own: {text}"
        );
    }

    #[test]
    fn wake_splits_paste_or_literal_from_submit() {
        assert_eq!(
            &paste_args("%7", "COLLAB_NOTIFY message", "collab-wake-test")[..],
            &[
                "set-buffer",
                "-b",
                "collab-wake-test",
                "COLLAB_NOTIFY message",
                ";",
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                "collab-wake-test",
                "-t",
                "%7"
            ][..]
        );
        assert_eq!(
            &literal_args("%7", "COLLAB_NOTIFY message")[..],
            &["send-keys", "-t", "%7", "-l", "--", "COLLAB_NOTIFY message"][..]
        );
        let paste_submit =
            paste_submit_args("%7", "COLLAB_NOTIFY message", "collab-wake-test");
        assert!(paste_submit.windows(4).any(|w| w == ["paste-buffer", "-p", "-d", "-b"]));
        assert!(
            paste_submit.windows(4).any(|w| w == ["send-keys", "-t", "%7", "C-m"]),
            "{paste_submit:?}"
        );
        assert!(
            !paste_submit.iter().any(|a| a.contains("sleep")),
            "Codex Enter must share the paste queue, not a delayed process: {paste_submit:?}"
        );
        assert_eq!(
            submit_kind(
                "node",
                "  Cursor Grok 4.6 High Fast · 54.5%\n  /tmp/project · main\n"
            ),
            SubmitKind::Literal
        );
        assert_eq!(submit_kind("agent", ""), SubmitKind::Literal);
        assert_eq!(submit_kind("codex", ""), SubmitKind::BracketedPaste);
    }

    #[test]
    fn probe_requires_agent_tui_before_idle_or_working() {
        assert_eq!(
            agent_state_from("zsh", "Macstudio.local", ""),
            AgentState::Unknown
        );
        assert_eq!(agent_state_from("node", "", ""), AgentState::Unknown);
        assert_eq!(
            agent_state_from("node", "Word Counter", ""),
            AgentState::Unknown
        );
        assert_eq!(
            agent_state_from("node", "⠋ routecodex", ""),
            AgentState::Working
        );
        assert_eq!(agent_state_from("codex", "collab", ""), AgentState::Waiting);
        assert_eq!(
            agent_state_from("codex", "⠋ collab", ""),
            AgentState::Working
        );
        assert_eq!(
            agent_state_from("node", "Cursor Agent", ""),
            AgentState::Unknown
        );
        assert_eq!(
            agent_state_from(
                "node",
                "Word Counter",
                "  Cursor Grok 4.6 High Fast · 54.5%\n"
            ),
            AgentState::Unknown
        );
        assert_eq!(
            agent_state_from("node", "Word Counter", "  Run Everything\n"),
            AgentState::Unknown
        );
        assert_eq!(
            agent_state_from("node", "Word Counter", "  /tmp/cursor-cli-cap · main\n"),
            AgentState::Unknown
        );
        assert_eq!(
            agent_state_from(
                "node",
                "Word Counter",
                "  Cursor Grok 4.6 High Fast · 54.5%\n  /tmp/cursor-cli-cap · main\n"
            ),
            AgentState::Waiting
        );
        assert_eq!(
            agent_state_from(
                "node",
                "Word Counter",
                " ⠘⠆ Working\n  Cursor Grok 4.6 High Fast · 54.5% · 2 files edited          Run Everything\n"
            ),
            AgentState::Working
        );
        assert_eq!(
            agent_state_from(
                "node",
                "Word Counter",
                "  → Add a follow-up\n  Cursor Grok 4.6 High Fast · 54.5% · 2 files edited          Run Everything\n"
            ),
            AgentState::Waiting
        );
    }
}

pub fn knock_or_log(log: &Path, pane: &str, text: &str) -> bool {
    if let Err(e) = knock(pane, text) {
        append_log(log, &format!("knock failed pane={} err={}", pane, e));
        false
    } else {
        true
    }
}

pub fn append_log(log: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = writeln!(
            f,
            "{} {}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
            line
        );
    }
}
