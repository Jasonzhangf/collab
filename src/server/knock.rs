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
    Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_current_command}\t#{pane_title}",
        ])
        .output()
        .ok()
        .map(|o| {
            if !o.status.success() {
                return AgentState::Absent;
            }
            let output = String::from_utf8_lossy(&o.stdout);
            let Some((command, title)) = output.trim().split_once('\t') else {
                return AgentState::Unknown;
            };
            agent_state_from(command, title)
        })
        .unwrap_or(AgentState::Unknown)
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

fn agent_state_from(command: &str, title: &str) -> AgentState {
    if !matches!(command, "node" | "codex" | "claude" | "agy" | "dsh") {
        return AgentState::Absent;
    }
    let title = title.trim();
    if title.is_empty() {
        return AgentState::Unknown;
    }
    if title
        .chars()
        .next()
        .is_some_and(|first| ('\u{2800}'..='\u{28ff}').contains(&first))
    {
        AgentState::Working
    } else {
        AgentState::Waiting
    }
}

fn wake_args<'a>(pane: &'a str, text: &'a str, buffer: &'a str) -> Vec<&'a str> {
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
        ";",
        "send-keys",
        "-t",
        pane,
        "C-m",
    ]
}

pub fn knock(pane: &str, text: &str) -> anyhow::Result<()> {
    if !pane_alive(pane) {
        anyhow::bail!("pane {} not alive", pane);
    }
    if !pane_accepts_notification(pane) {
        anyhow::bail!("pane {} has no known agent", pane);
    }
    let sequence = WAKE_BUFFER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let buffer = format!("collab-wake-{}-{sequence}", std::process::id());
    let sent = Command::new("tmux")
        .args(wake_args(pane, text, &buffer))
        .status()?;
    if !sent.success() {
        anyhow::bail!("tmux notification delivery failed for pane {}", pane);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{agent_state_from, wake_args, AgentState};

    #[test]
    #[ignore = "requires tmux and node; uses a disposable session"]
    fn live_working_pane_receives_one_batch_with_enter() {
        use std::process::Command;
        let session = format!("collab-batch-test-{}", std::process::id());
        let output = Command::new("tmux").args([
            "new-session", "-d", "-P", "-F", "#{pane_id}", "-s", &session,
            "node -e 'process.stdin.setRawMode(true);process.stdin.resume();process.stdin.on(\"data\",b=>{process.stdout.write(\"RX:\"+b.toString(\"hex\")+\"\\n\")})'"
        ]).output().unwrap();
        assert!(output.status.success());
        let pane = String::from_utf8(output.stdout).unwrap().trim().to_string();
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", &self.0])
                    .status();
            }
        }
        let _cleanup = Cleanup(session);
        std::thread::sleep(std::time::Duration::from_millis(300));
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
        std::thread::sleep(std::time::Duration::from_millis(300));
        let capture = Command::new("tmux")
            .args(["capture-pane", "-p", "-J", "-t", &pane])
            .output()
            .unwrap();
        let text = String::from_utf8(capture.stdout).unwrap();
        let hex = text
            .lines()
            .filter_map(|l| l.strip_prefix("RX:"))
            .collect::<String>();
        assert!(hex.contains("6f6e65205b66697273745d"));
        assert!(hex.contains("74776f205b7365636f6e645d"));
        assert_eq!(hex.matches("0d").count(), 1, "{text}");
    }

    #[test]
    fn wake_is_one_tmux_command_queue_with_bracketed_paste_and_submit() {
        assert_eq!(
            &wake_args("%7", "COLLAB_NOTIFY message", "collab-wake-test")[..],
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
                "%7",
                ";",
                "send-keys",
                "-t",
                "%7",
                "C-m",
            ][..]
        );
    }

    #[test]
    fn probe_distinguishes_absent_unknown_working_and_waiting() {
        assert_eq!(
            agent_state_from("zsh", "Macstudio.local"),
            AgentState::Absent
        );
        assert_eq!(agent_state_from("node", ""), AgentState::Unknown);
        assert_eq!(
            agent_state_from("node", "⠋ routecodex"),
            AgentState::Working
        );
        assert_eq!(agent_state_from("codex", "collab"), AgentState::Waiting);
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
