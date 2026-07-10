//! config.toml の読み込み。
//!
//! 論理デバイスごとに操作 → 実行コマンド配列を持つ。本体はこの配列を
//! そのまま exec するだけ（バックエンド非依存）。enl → casa の移行は
//! コード変更ではなく config の差し替えで済む。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// デバイス種別。UI の動詞と正規化のディスパッチに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Shutter,
    Light,
}

/// light 用の名前付きプリセット（完成済みコマンド配列）。
/// 色・kelvin の任意値入力は作らない — config に並べたものだけ実行できる。
#[derive(Debug, Clone, Deserialize)]
pub struct Preset {
    /// URL に使う識別子。
    pub name: String,
    /// UI 表示名（任意）。未指定なら name。
    #[serde(default, alias = "alias")]
    pub label: Option<String>,
    /// UI の色玉スウォッチ用 CSS color（任意）。未指定はテキストチップ表示。
    /// 形式検証はしない — config を書けるのは設置者本人のみ。
    #[serde(default)]
    pub color: Option<String>,
    /// exec するコマンド配列。
    pub cmd: Vec<String>,
}

impl Preset {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default, rename = "device")]
    pub devices: Vec<Device>,
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
}

/// 複数デバイスをまとめて一括操作するためのグループ。
#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    /// グループ識別子（URL に使う）。
    pub name: String,
    /// 表示名（任意）。`label` でも `alias` でも書ける。未指定なら name。
    #[serde(default, alias = "alias")]
    pub label: Option<String>,
    /// メンバーの device 名。記載順に操作する。
    pub members: Vec<String>,
}

impl Group {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    /// 論理名。URL とフロントが使う安定識別子。
    pub name: String,
    /// 表示名（任意）。未指定なら name。config では `label` でも `alias` でも書ける。
    #[serde(default, alias = "alias")]
    pub label: Option<String>,
    /// デバイス種別。省略時 shutter（既存 config 互換）。
    #[serde(default)]
    pub kind: Kind,
    /// 状態取得コマンド。全 kind で必須。
    pub get_state: Vec<String>,
    /// open コマンド（shutter 必須 / light 不可）。
    #[serde(default)]
    pub open: Option<Vec<String>>,
    /// close コマンド（shutter 必須 / light 不可）。
    #[serde(default)]
    pub close: Option<Vec<String>>,
    /// stop コマンド（shutter 任意 / light 不可）。
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// on コマンド（light 必須 / shutter 不可）。
    #[serde(default)]
    pub on: Option<Vec<String>>,
    /// off コマンド（light 必須 / shutter 不可）。
    #[serde(default)]
    pub off: Option<Vec<String>>,
    /// 任意色コマンドテンプレ（light のみ・任意）。{color} プレースホルダを
    /// 配列全体でちょうど 1 個含み、検証済み hex（例 "#ff69b4"）に置換して exec される。
    #[serde(default)]
    pub color: Option<Vec<String>>,
    /// 色・色温度プリセット（light のみ）。
    #[serde(default, rename = "preset")]
    pub presets: Vec<Preset>,
}

impl Device {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    pub fn open_cmd(&self) -> Option<&[String]> {
        self.open.as_deref()
    }

    pub fn close_cmd(&self) -> Option<&[String]> {
        self.close.as_deref()
    }

    /// stop に対応していれば、その exec コマンドを返す。
    pub fn stop_cmd(&self) -> Option<&[String]> {
        self.stop.as_deref()
    }

    pub fn on_cmd(&self) -> Option<&[String]> {
        self.on.as_deref()
    }

    pub fn off_cmd(&self) -> Option<&[String]> {
        self.off.as_deref()
    }

    pub fn preset_cmd(&self, name: &str) -> Option<&[String]> {
        self.presets
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.cmd.as_slice())
    }

    pub fn color_cmd(&self) -> Option<&[String]> {
        self.color.as_deref()
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read(std::io::Error),
    Parse(toml::de::Error),
    Empty,
    DuplicateName(String),
    EmptyCommand(String),
    EmptyGroup(String),
    UnknownMember { group: String, member: String },
    DuplicateGroup(String),
    MissingCommand { device: String, field: &'static str },
    ForbiddenField { device: String, field: &'static str },
    DuplicatePreset { device: String, preset: String },
    LightInGroup { group: String, member: String },
    DuplicateGroupMember { device: String },
    ColorPlaceholder { device: String, count: usize },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(e) => write!(f, "config 読み込み失敗: {e}"),
            ConfigError::Parse(e) => write!(f, "config パース失敗: {e}"),
            ConfigError::Empty => write!(f, "config に [[device]] が 1 つもない"),
            ConfigError::DuplicateName(n) => write!(f, "device 名が重複: {n}"),
            ConfigError::EmptyCommand(n) => {
                write!(f, "device {n}: コマンド配列が空")
            }
            ConfigError::EmptyGroup(n) => write!(f, "group {n}: members が空"),
            ConfigError::UnknownMember { group, member } => {
                write!(f, "group {group}: 未知の device を参照: {member}")
            }
            ConfigError::DuplicateGroup(n) => write!(f, "group 名が重複: {n}"),
            ConfigError::MissingCommand { device, field } => {
                write!(f, "device {device}: {field} がない（この kind では必須）")
            }
            ConfigError::ForbiddenField { device, field } => {
                write!(f, "device {device}: {field} はこの kind では指定できない")
            }
            ConfigError::DuplicatePreset { device, preset } => {
                write!(f, "device {device}: preset 名が重複: {preset}")
            }
            ConfigError::LightInGroup { group, member } => {
                write!(f, "group {group}: light はグループに入れられない: {member}")
            }
            ConfigError::DuplicateGroupMember { device } => {
                write!(f, "device {device}: 複数のグループに所属できない（UI が個別行をデバイスごとに 1 つしか持てないため）")
            }
            ConfigError::ColorPlaceholder { device, count } => {
                write!(f, "device {device}: color テンプレは {{color}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// kind に応じた必須コマンドの検査。None は Missing、空配列は Empty。
fn require(
    device: &str,
    field: &'static str,
    v: &Option<Vec<String>>,
) -> Result<(), ConfigError> {
    match v {
        Some(c) if !c.is_empty() => Ok(()),
        Some(_) => Err(ConfigError::EmptyCommand(device.to_string())),
        None => Err(ConfigError::MissingCommand {
            device: device.to_string(),
            field,
        }),
    }
}

/// この kind では書けないフィールドの検査。
fn forbid(
    device: &str,
    field: &'static str,
    v: &Option<Vec<String>>,
) -> Result<(), ConfigError> {
    if v.is_some() {
        Err(ConfigError::ForbiddenField {
            device: device.to_string(),
            field,
        })
    } else {
        Ok(())
    }
}

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
            if d.get_state.is_empty() {
                return Err(ConfigError::EmptyCommand(d.name.clone()));
            }
            match d.kind {
                Kind::Shutter => {
                    require(&d.name, "open", &d.open)?;
                    require(&d.name, "close", &d.close)?;
                    forbid(&d.name, "on", &d.on)?;
                    forbid(&d.name, "off", &d.off)?;
                    forbid(&d.name, "color", &d.color)?;
                    if !d.presets.is_empty() {
                        return Err(ConfigError::ForbiddenField {
                            device: d.name.clone(),
                            field: "preset",
                        });
                    }
                    // stop は任意だが、指定するなら空配列は不可。
                    if d.stop.as_ref().is_some_and(|s| s.is_empty()) {
                        return Err(ConfigError::EmptyCommand(d.name.clone()));
                    }
                }
                Kind::Light => {
                    require(&d.name, "on", &d.on)?;
                    require(&d.name, "off", &d.off)?;
                    forbid(&d.name, "open", &d.open)?;
                    forbid(&d.name, "close", &d.close)?;
                    forbid(&d.name, "stop", &d.stop)?;
                    let mut pseen = std::collections::HashSet::new();
                    for p in &d.presets {
                        if !pseen.insert(&p.name) {
                            return Err(ConfigError::DuplicatePreset {
                                device: d.name.clone(),
                                preset: p.name.clone(),
                            });
                        }
                        if p.cmd.is_empty() {
                            return Err(ConfigError::EmptyCommand(d.name.clone()));
                        }
                    }
                    if let Some(color) = &d.color {
                        if color.is_empty() {
                            return Err(ConfigError::EmptyCommand(d.name.clone()));
                        }
                        let count: usize =
                            color.iter().map(|s| s.matches("{color}").count()).sum();
                        if count != 1 {
                            return Err(ConfigError::ColorPlaceholder {
                                device: d.name.clone(),
                                count,
                            });
                        }
                    }
                }
            }
        }

        let mut seen_g = std::collections::HashSet::new();
        let mut seen_m = std::collections::HashSet::new();
        for g in &self.groups {
            if !seen_g.insert(&g.name) {
                return Err(ConfigError::DuplicateGroup(g.name.clone()));
            }
            if g.members.is_empty() {
                return Err(ConfigError::EmptyGroup(g.name.clone()));
            }
            for m in &g.members {
                match self.find(m) {
                    None => {
                        return Err(ConfigError::UnknownMember {
                            group: g.name.clone(),
                            member: m.clone(),
                        })
                    }
                    // グループは当面シャッター専用（一括開閉の意味論が light に合わない）。
                    Some(d) if d.kind == Kind::Light => {
                        return Err(ConfigError::LightInGroup {
                            group: g.name.clone(),
                            member: m.clone(),
                        })
                    }
                    Some(_) => {}
                }
                // デバイスが複数グループに所属していないかチェック
                if !seen_m.insert(m) {
                    return Err(ConfigError::DuplicateGroupMember {
                        device: m.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.name == name)
    }

    pub fn find_group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
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
            r##"
            bind = "127.0.0.1:9999"
            [[device]]
            name = "shutter"
            label = "シャッター"
            get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
            open = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "close"]
            "##,
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
    fn alias_key_works_as_label() {
        let p = write_tmp(
            "alias",
            r##"
            [[device]]
            name = "shutter1"
            alias = "リビング"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("shutter1").unwrap().label(), "リビング");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn group_parsed_and_validated() {
        let p = write_tmp(
            "group",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl","get","x","026301","open_close_state"]
            open = ["enl","set","x","026301","open_close_operation","open"]
            close = ["enl","set","x","026301","open_close_operation","close"]
            [[device]]
            name = "s2"
            get_state = ["enl","get","x","026302","open_close_state"]
            open = ["enl","set","x","026302","open_close_operation","open"]
            close = ["enl","set","x","026302","open_close_operation","close"]
            [[group]]
            name = "all"
            alias = "全部"
            members = ["s1","s2"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let g = cfg.find_group("all").unwrap();
        assert_eq!(g.label(), "全部");
        assert_eq!(g.members, vec!["s1", "s2"]);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn group_rejects_unknown_member() {
        let p = write_tmp(
            "badgroup",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl","get","x","026301","open_close_state"]
            open = ["enl","set","x","026301","open_close_operation","open"]
            close = ["enl","set","x","026301","open_close_operation","close"]
            [[group]]
            name = "all"
            members = ["s1","ghost"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::UnknownMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_device_parses() {
        let p = write_tmp(
            "light",
            r##"
            [[device]]
            name  = "living_lights"
            alias = "リビング照明"
            kind  = "light"
            get_state = ["mat", "read", "--node", "5", "--cluster", "onoff", "--attribute", "on-off"]
            on  = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            color = "#ffd9a0"
            cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]
            [[device.preset]]
            name  = "pink"
            cmd   = ["mat", "color", "--node", "5", "--name", "pink"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let d = cfg.find("living_lights").unwrap();
        assert_eq!(d.kind, Kind::Light);
        assert_eq!(d.label(), "リビング照明");
        assert_eq!(d.on_cmd().unwrap()[1], "on");
        assert_eq!(d.off_cmd().unwrap()[1], "off");
        assert_eq!(d.preset_cmd("warm").unwrap().last().unwrap(), "2700");
        assert!(d.preset_cmd("nope").is_none());
        assert_eq!(d.presets[0].label(), "電球色");
        assert_eq!(d.presets[1].label(), "pink"); // label 未指定は name
        assert_eq!(d.presets[0].color.as_deref(), Some("#ffd9a0"));
        assert_eq!(d.presets[1].color, None); // color 未指定は None
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn default_kind_is_shutter() {
        let p = write_tmp(
            "defkind",
            r##"
            [[device]]
            name = "shutter"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("shutter").unwrap().kind, Kind::Shutter);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_requires_on_off() {
        let p = write_tmp(
            "lightreq",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::MissingCommand { field: "off", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_rejects_shutter_fields() {
        let p = write_tmp(
            "lightforbid",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "open", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn shutter_rejects_light_fields() {
        let p = write_tmp(
            "shutterforbid",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            on = ["mat", "on", "--node", "5"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "on", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn preset_duplicate_name_rejected() {
        let p = write_tmp(
            "presetdup",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            [[device.preset]]
            name = "warm"
            cmd = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]
            [[device.preset]]
            name = "warm"
            cmd = ["mat", "color-temp", "--node", "5", "--kelvin", "3000"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicatePreset { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn preset_empty_cmd_rejected() {
        let p = write_tmp(
            "presetempty",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            [[device.preset]]
            name = "warm"
            cmd = []
            "##,
        );
        assert!(matches!(Config::load(&p), Err(ConfigError::EmptyCommand(_))));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_in_group_rejected() {
        let p = write_tmp(
            "lightgroup",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            [[group]]
            name = "all"
            members = ["s1", "l1"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::LightInGroup { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn default_label_is_name() {
        let d = Device {
            name: "x".into(),
            label: None,
            kind: Kind::Shutter,
            get_state: vec!["a".into()],
            open: Some(vec!["a".into()]),
            close: Some(vec!["a".into()]),
            stop: None,
            on: None,
            off: None,
            color: None,
            presets: vec![],
        };
        assert_eq!(d.label(), "x");
        assert!(d.stop_cmd().is_none());
    }

    #[test]
    fn stop_optional_and_parsed() {
        let p = write_tmp(
            "stop",
            r##"
            [[device]]
            name = "shutter"
            get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
            open = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "close"]
            stop = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "stop"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let d = cfg.find("shutter").unwrap();
        assert_eq!(d.stop_cmd().unwrap().last().unwrap(), "stop");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn rejects_empty_stop() {
        let p = write_tmp(
            "emptystop",
            r##"
            [[device]]
            name = "shutter"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            stop = []
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::EmptyCommand(_))
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn device_in_multiple_groups_rejected() {
        let p = write_tmp(
            "dupgroupmember",
            r##"
            [[device]]
            name = "shutter"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[group]]
            name = "group1"
            members = ["shutter"]
            [[group]]
            name = "group2"
            members = ["shutter"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateGroupMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn device_duplicate_within_group_rejected() {
        let p = write_tmp(
            "dupwithingroup",
            r##"
            [[device]]
            name = "shutter"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[group]]
            name = "all"
            members = ["shutter", "shutter"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateGroupMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn color_template_parses() {
        let p = write_tmp(
            "colorok",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            color = ["mat", "color", "--node", "5", "--rgb", "{color}"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let d = cfg.find("l1").unwrap();
        assert_eq!(d.color_cmd().unwrap().last().unwrap(), "{color}");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn color_placeholder_zero_rejected() {
        let p = write_tmp(
            "colorzero",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            color = ["mat", "color", "--node", "5", "--rgb", "red"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ColorPlaceholder { count: 0, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn color_placeholder_two_rejected() {
        let p = write_tmp(
            "colortwo",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            color = ["mat", "color", "--node", "5", "--rgb", "{color}", "--x", "{color}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ColorPlaceholder { count: 2, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn color_on_shutter_rejected() {
        let p = write_tmp(
            "colorshutter",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            color = ["mat", "color", "--node", "5", "--rgb", "{color}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "color", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn color_empty_rejected() {
        let p = write_tmp(
            "colorempty",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            color = []
            "##,
        );
        assert!(matches!(Config::load(&p), Err(ConfigError::EmptyCommand(_))));
        std::fs::remove_file(p).ok();
    }
}
