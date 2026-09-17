#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Unknown,
    Working,
    Waiting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityPresence {
    Present,
    Missing,
    Unknown,
}

pub fn append_log(path: &std::path::Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{} {}", chrono::Utc::now().to_rfc3339(), text);
    }
}
