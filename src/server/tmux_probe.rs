use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default)]
struct Sample {
    fingerprint: String,
    unchanged: u8,
    alerted: bool,
}

static SAMPLES: OnceLock<Mutex<HashMap<String, Sample>>> = OnceLock::new();

fn samples() -> &'static Mutex<HashMap<String, Sample>> {
    SAMPLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(pane: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane, "-S", "-"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Ignore common clock/status-bar lines so a changing clock cannot hide a
    // frozen TUI. The remaining rendered pane is the probe's truth sample.
    let stable = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.contains("Last login:")
                && !trimmed.contains("Worked for")
                && !trimmed
                    .chars()
                    .all(|c| c.is_ascii_digit() || ":-/. ".contains(c))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(stable)
}

/// Return panes that appear frozen after three unchanged 5-second samples.
/// A pane is returned once per freeze episode; a changed sample clears it.
pub fn poll(panes: &[String]) -> Vec<(String, &'static str)> {
    let mut state = samples().lock().unwrap();
    let mut alerts = Vec::new();
    for pane in panes {
        let Some(current) = fingerprint(pane) else {
            continue;
        };
        let entry = state.entry(pane.clone()).or_default();
        if entry.fingerprint.is_empty() {
            entry.fingerprint = current;
            entry.unchanged = 1;
            continue;
        }
        if entry.fingerprint == current {
            entry.unchanged = entry.unchanged.saturating_add(1);
        } else {
            let was_alerted = entry.alerted;
            entry.fingerprint = current;
            entry.unchanged = 0;
            entry.alerted = false;
            if was_alerted {
                alerts.push((pane.clone(), "tmux_rendering_resumed"));
            }
        }
        if entry.unchanged >= 3 && !entry.alerted {
            entry.alerted = true;
            alerts.push((pane.clone(), "tmux_rendering_stalled"));
        }
    }
    state.retain(|pane, _| panes.iter().any(|current| current == pane));
    alerts
}
