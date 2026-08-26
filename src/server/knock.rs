use std::path::Path;
use std::process::Command;

/// Best-effort tmux knock. Never fails the caller's operation: delivery truth
/// lives in the mailbox, the knock is only a wake-up signal.
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
        .args(["send-keys", "-t", pane, "Enter"])
        .status()?;
    if !submitted.success() {
        anyhow::bail!("tmux Enter delivery failed for pane {}", pane);
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
