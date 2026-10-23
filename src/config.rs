use serde::Deserialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

fn remote() -> String {
    "origin".into()
}
fn lock_name() -> String {
    "git-rsrs.lock".into()
}
fn quiet() -> Duration {
    Duration::from_secs(30)
}
fn interval() -> Duration {
    Duration::from_secs(300)
}
fn git_timeout() -> Duration {
    Duration::from_secs(120)
}
fn duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let raw = String::deserialize(d)?;
    let value = humantime::parse_duration(&raw).map_err(serde::de::Error::custom)?;
    if value.is_zero() {
        return Err(serde::de::Error::custom("duration must be positive"));
    }
    // Checked Instant arithmetic must remain representable on supported hosts.
    if value > Duration::from_secs(365 * 24 * 3600) {
        return Err(serde::de::Error::custom("duration exceeds one year"));
    }
    Ok(value)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub branch: String,
    #[serde(default = "remote")]
    pub remote: String,
    #[serde(default = "lock_name")]
    pub lock_name: String,
    #[serde(default = "quiet", deserialize_with = "duration")]
    pub quiet_period: Duration,
    #[serde(default = "interval", deserialize_with = "duration")]
    pub interval: Duration,
    #[serde(default = "git_timeout", deserialize_with = "duration")]
    pub git_timeout: Duration,
    #[serde(default = "quiet", deserialize_with = "duration")]
    pub lock_timeout: Duration,
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(skip)]
    pub repo: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hooks {
    pub pre_commit: Vec<HookConfig>,
    pub post_integrate: Vec<HookConfig>,
    pub on_result: Vec<HookConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    pub command: Vec<String>,
    #[serde(default = "quiet", deserialize_with = "duration")]
    pub timeout: Duration,
    #[serde(default)]
    pub reject_exit_codes: Vec<i32>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::parse(&fs::read_to_string(path)?)
    }
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text)?;
        let invalid = |message: &str| ConfigError::Invalid(message.into());
        if config.version != 1 {
            return Err(invalid("unsupported schema version; expected 1"));
        }
        if config.branch.trim().is_empty() || config.branch.starts_with('-') {
            return Err(invalid("branch must be nonempty and cannot start with '-'"));
        }
        if config.remote.trim().is_empty() || config.remote.starts_with('-') {
            return Err(invalid("remote must be nonempty and cannot start with '-'"));
        }
        if config.lock_name.is_empty()
            || config.lock_name.contains(['/', '\\'])
            || config.lock_name.contains("..")
            || config.lock_name == "."
        {
            return Err(invalid("lock_name must be a plain filename"));
        }
        for hook in config
            .hooks
            .pre_commit
            .iter()
            .chain(&config.hooks.post_integrate)
            .chain(&config.hooks.on_result)
        {
            if hook.command.is_empty() || hook.command[0].is_empty() {
                return Err(invalid("hook command must contain an executable"));
            }
            let mut codes = HashSet::new();
            for code in &hook.reject_exit_codes {
                if !(1..=255).contains(code) || !codes.insert(code) {
                    return Err(invalid(
                        "rejection exit codes must be unique and between 1 and 255",
                    ));
                }
            }
        }
        Ok(config)
    }
}
