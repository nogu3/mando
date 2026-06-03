//! config.toml の読み込み。
//!
//! 論理デバイスごとに操作 → 実行コマンド配列を持つ。本体はこの配列を
//! そのまま exec するだけ（バックエンド非依存）。enl → casa の移行は
//! コード変更ではなく config の差し替えで済む。

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default, rename = "device")]
    pub devices: Vec<Device>,
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    /// 論理名。URL とフロントが使う安定識別子。
    pub name: String,
    /// 表示名（任意）。未指定なら name。
    #[serde(default)]
    pub label: Option<String>,
    /// 状態取得コマンド。例: ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
    pub get_state: Vec<String>,
    /// open コマンド。
    pub open: Vec<String>,
    /// close コマンド。
    pub close: Vec<String>,
}

impl Device {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read(std::io::Error),
    Parse(toml::de::Error),
    Empty,
    DuplicateName(String),
    EmptyCommand(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(e) => write!(f, "config 読み込み失敗: {e}"),
            ConfigError::Parse(e) => write!(f, "config パース失敗: {e}"),
            ConfigError::Empty => write!(f, "config に [[device]] が 1 つもない"),
            ConfigError::DuplicateName(n) => write!(f, "device 名が重複: {n}"),
            ConfigError::EmptyCommand(n) => {
                write!(
                    f,
                    "device {n}: コマンド配列が空。get_state/open/close すべて必須"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(ConfigError::Read)?;
        let cfg: Config = toml::from_str(&text).map_err(ConfigError::Parse)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.devices.is_empty() {
            return Err(ConfigError::Empty);
        }
        let mut seen = std::collections::HashSet::new();
        for d in &self.devices {
            if !seen.insert(&d.name) {
                return Err(ConfigError::DuplicateName(d.name.clone()));
            }
            if d.get_state.is_empty() || d.open.is_empty() || d.close.is_empty() {
                return Err(ConfigError::EmptyCommand(d.name.clone()));
            }
        }
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("mando_cfg_{}_{tag}.toml", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn loads_valid_config() {
        let p = write_tmp(
            "valid",
            r#"
            bind = "127.0.0.1:9999"
            [[device]]
            name = "shutter"
            label = "シャッター"
            get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
            open = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "close"]
            "#,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:9999");
        assert_eq!(cfg.devices.len(), 1);
        assert_eq!(cfg.find("shutter").unwrap().label(), "シャッター");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn rejects_empty() {
        let p = write_tmp("empty", "bind = \"0.0.0.0:8080\"\n");
        assert!(matches!(Config::load(&p), Err(ConfigError::Empty)));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn default_label_is_name() {
        let d = Device {
            name: "x".into(),
            label: None,
            get_state: vec!["a".into()],
            open: vec!["a".into()],
            close: vec!["a".into()],
        };
        assert_eq!(d.label(), "x");
    }
}
