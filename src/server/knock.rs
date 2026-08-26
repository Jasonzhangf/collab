use std::path::Path;
use std::process::Command;

/// Live tmux prompt for the recipient pane. Mailbox truth remains the
/// authoritative delivery record; this knock only wakes the recipient with
/// the reasoning prompt. Text + carriage return are two distinct `tmux
/// send-keys` calls. This matches zterm v1: literal text uses `--`, then the
/// tmux `Enter` key submits the prompt.
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
    let sent = Command::new("tmux")
        .args(literal_args(pane, text))
        .status()?;
    if !sent.success() {
        anyhow::bail!("tmux text delivery failed for pane {}", pane);
    }
    let submitted = Command::new("tmux").args(submit_args(pane)).status()?;
    if !submitted.success() {
        anyhow::bail!("tmux Enter delivery failed for pane {}", pane);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{literal_args, submit_args};

    #[test]
    fn matches_zterm_v1_literal_then_enter() {
        assert_eq!(
            literal_args("%7", "[MAIL] body"),
            ["send-keys", "-t", "%7", "-l", "--", "[MAIL] body"]
        );
        assert_eq!(submit_args("%7"), ["send-keys", "-t", "%7", "Enter"]);
    }
}

pub fn knock_or_log(log: &Path, pane: &str, text: &str) {
    if let Err(e) = knock(pane, text) {
        append_log(log, &format!("knock failed pane={} err={}", pane, e));
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
