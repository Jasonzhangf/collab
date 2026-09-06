use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub notifications: Notifications,
    pub timers: Timers,
    pub subagent: Subagent,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            notifications: Notifications::default(),
            timers: Timers::default(),
            subagent: Subagent::default(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notifications {
    pub enabled: bool,
    pub mode: String,
    pub batch_window_seconds: u64,
    pub transport: String,
    pub submit_enter: bool,
    pub events: BTreeMap<String, EventPolicy>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventPolicy {
    pub mode: String,
}
impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: "batch".into(),
            batch_window_seconds: 60,
            transport: "tmux".into(),
            submit_enter: true,
            events: BTreeMap::from([(
                "deadline".into(),
                EventPolicy {
                    mode: "immediate".into(),
                },
            )]),
        }
    }
}
impl Notifications {
    pub fn delay_ms(&self, event: &str) -> i64 {
        let key = event.replace('-', "_");
        let mode = self
            .events
            .get(&key)
            .map(|p| p.mode.as_str())
            .filter(|s| *s != "inherit")
            .unwrap_or(&self.mode);
        if mode == "immediate" {
            0
        } else {
            self.batch_window_seconds as i64 * 1000
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Timers {
    pub enabled: bool,
    pub tick_interval_ms: u64,
}
impl Default for Timers {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_interval_ms: 1000,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Subagent {
    pub profile_priority: Vec<String>,
    pub persistent: bool,
    pub close_on_task_complete: bool,
    pub profiles: BTreeMap<String, Profile>,
    pub health: Health,
    pub startup: Startup,
    pub tmux: Tmux,
}
impl Default for Subagent {
    fn default() -> Self {
        Self {
            profile_priority: vec!["gcm".into(), "oauth".into()],
            persistent: true,
            close_on_task_complete: false,
            profiles: BTreeMap::from([
                (
                    "gcm".into(),
                    Profile {
                        codex_profile: "gcm".into(),
                        model: None,
                    },
                ),
                (
                    "oauth".into(),
                    Profile {
                        codex_profile: "oauth".into(),
                        model: Some("gpt-5.6-luna".into()),
                    },
                ),
            ]),
            health: Health::default(),
            startup: Startup::default(),
            tmux: Tmux::default(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub codex_profile: String,
    pub model: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Health {
    pub timeout_seconds: u64,
    pub attempts_per_profile: u64,
    pub expected_response: String,
}
impl Default for Health {
    fn default() -> Self {
        Self {
            timeout_seconds: 45,
            attempts_per_profile: 1,
            expected_response: "OK".into(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Startup {
    pub ready_timeout_seconds: u64,
}
impl Default for Startup {
    fn default() -> Self {
        Self {
            ready_timeout_seconds: 90,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tmux {
    pub name_template: String,
}
impl Default for Tmux {
    fn default() -> Self {
        Self {
            name_template: "{cwd_name}-subagent-{short_id}".into(),
        }
    }
}

pub fn path() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?)
            .join(".appsdk/config.toml"),
    )
}
fn merge(base: &mut toml::Value, overlay: toml::Value) {
    if let (Some(base), Some(overlay)) = (base.as_table_mut(), overlay.as_table()) {
        for (key, value) in overlay {
            if let Some(old) = base.get_mut(key) {
                merge(old, value.clone());
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = overlay;
    }
}
pub fn parse(text: &str, root: &Path) -> Result<Config> {
    let mut input: toml::Value = toml::from_str(text).context("invalid ~/.appsdk/config.toml")?;
    let projects = input
        .as_table_mut()
        .context("config must be a table")?
        .remove("projects");
    let mut merged = toml::Value::try_from(Config::default())?;
    merge(&mut merged, input);
    if let Some(projects) = projects {
        let mut matches = Vec::new();
        for project in projects
            .as_array()
            .context("projects must be an array of tables")?
        {
            let mut project = project.clone();
            let table = project
                .as_table_mut()
                .context("project override must be a table")?;
            let path = table
                .remove("root")
                .and_then(|v| v.as_str().map(PathBuf::from))
                .context("project root is required")?;
            if !path.is_absolute() {
                bail!("project override root must be absolute");
            }
            let path = path.canonicalize().unwrap_or(path);
            if root.starts_with(&path) {
                matches.push((path, project));
            }
        }
        matches.sort_by_key(|(path, _)| path.components().count());
        for pair in matches.windows(2) {
            if pair[0].0 == pair[1].0 {
                bail!("duplicate project override root");
            }
        }
        for (_, project) in matches {
            merge(&mut merged, project);
        }
    }
    let config: Config = merged.try_into()?;
    config.validate()?;
    Ok(config)
}
impl Config {
    fn validate(&self) -> Result<()> {
        let n = &self.notifications;
        if !matches!(n.mode.as_str(), "immediate" | "batch")
            || !(1..=3600).contains(&n.batch_window_seconds)
        {
            bail!("invalid notification mode/window");
        }
        if n.transport != "tmux" || !n.submit_enter {
            bail!("tmux with atomic Enter is the supported notification transport");
        }
        for (event, policy) in &n.events {
            if ![
                "direct_message",
                "resource_released",
                "async_result",
                "deadline",
            ]
            .contains(&event.as_str())
                || !["inherit", "immediate", "batch"].contains(&policy.mode.as_str())
            {
                bail!("invalid notification event policy: {event}");
            }
        }
        if !(100..=60000).contains(&self.timers.tick_interval_ms) {
            bail!("timer tick must be 100..60000ms");
        }
        let s = &self.subagent;
        if !s.persistent || s.close_on_task_complete {
            bail!("subagents currently require persistent=true and close_on_task_complete=false");
        }
        if s.health.attempts_per_profile != 1
            || !(1..=60).contains(&s.health.timeout_seconds)
            || s.health.expected_response.trim().is_empty()
        {
            bail!("health probe requires one attempt per profile and timeout 1..60s");
        }
        if !(1..=600).contains(&s.startup.ready_timeout_seconds)
            || !s.tmux.name_template.contains("{short_id}")
        {
            bail!("invalid startup timeout or tmux name template");
        }
        if s.profile_priority.is_empty() || s.profile_priority.len() > 4 {
            bail!("configure 1..4 profiles");
        }
        let mut seen = std::collections::BTreeSet::new();
        for name in &s.profile_priority {
            let p = s
                .profiles
                .get(name)
                .context("profile_priority names an undefined profile")?;
            if !seen.insert(name)
                || p.codex_profile.trim().is_empty()
                || p.codex_profile.starts_with('-')
            {
                bail!("invalid or duplicate profile");
            }
        }
        Ok(())
    }
}
pub fn load(root: &Path) -> Result<Config> {
    let path = path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e.into()),
    };
    let root = root.canonicalize()?;
    let git = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(&root)
        .output()?;
    let owner = if git.status.success() {
        let p = PathBuf::from(String::from_utf8_lossy(&git.stdout).trim());
        if p.file_name().is_some_and(|s| s == ".git") {
            p.parent().unwrap().to_path_buf()
        } else {
            root.clone()
        }
    } else {
        root.clone()
    };
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()?;
    let resolved = if top.status.success() {
        let top = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim());
        owner.join(root.strip_prefix(top).unwrap_or(Path::new("")))
    } else {
        owner
    };
    parse(&text, &resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_defaults_and_project_override() {
        let c = parse("", Path::new("/project")).unwrap();
        assert_eq!(c.notifications.delay_ms("direct-message"), 60000);
        assert_eq!(c.notifications.delay_ms("deadline"), 0);
        let c = parse("[[projects]]\nroot='/project'\n[projects.notifications]\nmode='immediate'\n[projects.subagent]\nprofile_priority=['oauth']", Path::new("/project")).unwrap();
        assert_eq!(c.notifications.delay_ms("direct-message"), 0);
        assert_eq!(c.subagent.profile_priority, vec!["oauth"]);
        assert_eq!(
            c.subagent.profiles["oauth"].model.as_deref(),
            Some("gpt-5.6-luna")
        );
    }
    #[test]
    fn rejects_unsafe_and_unknown_policy() {
        for s in [
            "[notifications]\nsubmit_enter=false",
            "[subagent.health]\nattempts_per_profile=99",
            "[notifications]\nmode='typo'",
            "[timers]\ntick_interval_ms=0",
        ] {
            assert!(parse(s, Path::new("/project")).is_err());
        }
    }
}
