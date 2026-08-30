use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub fn config_path(root: &Path) -> PathBuf {
    root.join(".agent-collab").join("collab.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(alias = "heartbeat_minutes")]
    pub continuation_minutes: i64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            continuation_minutes: 15,
        }
    }
}

pub fn load(root: &Path) -> anyhow::Result<Config> {
    let path = config_path(root);
    if !path.exists() {
        return Ok(Config::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let config: Config = serde_json::from_str(&content)?;
    validate(&config)?;
    Ok(config)
}

pub fn load_or_default(root: &Path) -> Config {
    load(root).unwrap_or_default()
}

pub fn save(root: &Path, config: &Config) -> anyhow::Result<()> {
    validate(config)?;
    let path = config_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{json}\n"))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn validate(config: &Config) -> anyhow::Result<()> {
    if config.continuation_minutes < 1 {
        anyhow::bail!("continuation_minutes must be >= 1");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_default() {
        let root = std::env::temp_dir();
        assert_eq!(load(&root).unwrap().continuation_minutes, 15);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let root = std::env::temp_dir().join(format!(
            "collab-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let config = Config {
            continuation_minutes: 90,
        };
        save(&root, &config).unwrap();
        assert_eq!(load(&root).unwrap().continuation_minutes, 90);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn invalid_continuation_interval_is_rejected() {
        let config = Config {
            continuation_minutes: 0,
        };
        assert!(save(std::path::Path::new("/tmp"), &config).is_err());
    }

    #[test]
    fn legacy_heartbeat_field_migrates_to_continuation() {
        let config: Config = serde_json::from_str(r#"{"heartbeat_minutes":7}"#).unwrap();
        assert_eq!(config.continuation_minutes, 7);
    }
}
