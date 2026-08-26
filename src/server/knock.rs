use std::path::Path;
use std::process::Command;

/// Live tmux prompt for the recipient pane. Mailbox truth remains the
/// authoritative delivery record; this knock only wakes the recipient with
/// the reasoning prompt. Text + carriage return are two distinct `tmux
/// send-keys` calls. Codex TUI consumes `C-m` as the submit key; the tmux
/// `Enter` key name can leave text in the editor instead of submitting it.
/// Live verification is done by a consumer harness in tests, not by
/// `capture-pane`, because terminal word-wrap makes visual line capture
/// unreliable for long prompts.
pub fn pane_alive(pane: &str) -> bool {
    Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, "-F", "#{pane_id}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn knock(pane: &str, text: &str) -> anyhow::Result<()> {
    if !pane_alive(pane) {
        anyhow::bail!("pane {} not alive", pane);
    }
    let sent = Command::new("tmux")
        .args(["send-keys", "-t", pane, "-l", text])
        .status()?;
    if !sent.success() {
        anyhow::bail!("tmux text delivery failed for pane {}", pane);
    }
    let submitted = Command::new("tmux")
        .args(["send-keys", "-t", pane, "C-m"])
        .status()?;
    if !submitted.success() {
        anyhow::bail!("tmux carriage-return delivery failed for pane {}", pane);
    }
    Ok(())
}

pub fn knock_or_log(log: &Path, pane: &str, text: &str) {
    if let Err(e) = knock(pane, text) {
        append_log(log, &format!("knock failed pane={} err={}", pane, e));
    }
}

pub fn append_log(log: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"), line);
    }
}
