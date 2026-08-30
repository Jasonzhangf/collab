use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

/// Live tmux prompt for the recipient pane. Mailbox truth remains the
/// authoritative delivery record; this knock only wakes the recipient with
/// the reasoning prompt. Text + carriage return are two distinct `tmux
/// send-keys` calls. This matches zterm v1: literal text uses `--`, then the
/// tmux `Enter` key submits the prompt.
/// Live verification is done by a consumer harness in tests, not by
/// `capture-pane`, because terminal word-wrap makes visual line capture
/// unreliable for long prompts.
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

pub fn pane_idle(pane: &str) -> bool {
    if !pane.starts_with('%') {
        return false;
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
            let output = String::from_utf8_lossy(&o.stdout);
            let Some((command, title)) = output.trim().split_once('\t') else {
                return false;
            };
            agent_waiting(command, title)
        })
        .unwrap_or(false)
}

fn agent_waiting(command: &str, title: &str) -> bool {
    if !matches!(command, "node" | "codex" | "claude" | "agy" | "dsh") {
        return false;
    }
    let title = title.trim();
    if title.is_empty() {
        return false;
    }
    !title
        .chars()
        .next()
        .is_some_and(|first| ('\u{2800}'..='\u{28ff}').contains(&first))
}

fn literal_args<'a>(pane: &'a str, text: &'a str) -> [&'a str; 6] {
    ["send-keys", "-t", pane, "-l", "--", text]
}

fn submit_args<'a>(pane: &'a str) -> [&'a str; 4] {
    ["send-keys", "-t", pane, "Enter"]
}

pub fn knock(pane: &str, text: &str) -> anyhow::Result<()> {
    if !pane_alive(pane) {
        anyhow::bail!("pane {} not alive", pane);
    }
    if !pane_idle(pane) {
        anyhow::bail!("pane {} is not a waiting agent", pane);
    }
    const SUBMIT_DELAY_MS: u64 = 2_000;
    let sent = Command::new("tmux")
        .args(literal_args(pane, text))
        .status()?;
    if !sent.success() {
        anyhow::bail!("tmux text delivery failed for pane {}", pane);
    }
    sleep(Duration::from_millis(SUBMIT_DELAY_MS));
    let submitted = Command::new("tmux").args(submit_args(pane)).status()?;
    if !submitted.success() {
        anyhow::bail!("tmux Enter delivery failed for pane {}", pane);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{agent_waiting, literal_args, submit_args};

    #[test]
    fn matches_zterm_v1_literal_then_enter() {
        assert_eq!(
            literal_args("%7", "[MAIL] body"),
            ["send-keys", "-t", "%7", "-l", "--", "[MAIL] body"]
        );
        assert_eq!(submit_args("%7"), ["send-keys", "-t", "%7", "Enter"]);
    }

    #[test]
    fn wake_requires_waiting_agent_and_rejects_shell_or_working_agent() {
        assert!(agent_waiting("node", "routecodex"));
        assert!(agent_waiting("codex", "collab"));
        assert!(!agent_waiting("zsh", "Macstudio.local"));
        assert!(!agent_waiting("node", "⠋ routecodex"));
        assert!(!agent_waiting("node", ""));
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
