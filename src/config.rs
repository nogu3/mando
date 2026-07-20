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
    Switch,
}

/// switch の表示フェイス（表示専用。振る舞いは switch のまま）。UI のアイコン・
/// セクション・ラベルを切り替えるだけで、正規化や操作には影響しない。
// derive は既存 Kind と同じ Copy + PartialEq + Eq + Deserialize + Serialize。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Face {
    /// 💡 として「💡 照明」セクションに並べ、状態は「点灯/消灯」で出す。
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
    #[serde(default, rename = "graph")]
    pub graphs: Vec<Graph>,
    #[serde(default)]
    pub health: Option<Health>,
    /// デバイス exec の全体設定（任意）。
    #[serde(default)]
    pub exec: ExecSettings,
}

/// デバイス exec の全体設定。
#[derive(Debug, Clone, Deserialize)]
pub struct ExecSettings {
    /// デバイス exec（get_state / 操作）1 回の上限ミリ秒。超過は timeout 扱い。
    #[serde(default = "default_exec_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for ExecSettings {
    fn default() -> Self {
        ExecSettings {
            timeout_ms: default_exec_timeout_ms(),
        }
    }
}

fn default_exec_timeout_ms() -> u64 {
    15_000
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

/// embalse 等の読み出し CLI をテンプレで指定するグラフ定義（きろくセクション）。
/// mando はコマンド名を知らない — {period} を検証済み値に置換して exec するだけ
/// （設計原則 2: バックエンド非依存）。
#[derive(Debug, Clone, Deserialize)]
pub struct Graph {
    /// URL に使う識別子。
    pub name: String,
    /// UI 表示名（任意）。未指定なら name。
    #[serde(default, alias = "alias")]
    pub label: Option<String>,
    /// 今日ビュー（時系列カーブ）の単位表示。
    pub unit: String,
    /// 週/月ビュー（日別集計）の単位表示。未指定なら unit。
    #[serde(default)]
    pub unit_daily: Option<String>,
    /// exec するコマンドテンプレ。{period} プレースホルダをちょうど 1 個含む。
    pub query: Vec<String>,
    /// series 名（下層の生ラベル）→ UI 表示名の置換マップ（任意）。
    /// 下層固有の series 名の知識を config に留める（設計原則 2 と同型）。
    /// マップに無い series は素通し。検証はしない — 知らないキーは単に使われない。
    #[serde(default)]
    pub series_labels: Option<std::collections::HashMap<String, String>>,
}

impl Graph {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    /// period に応じた表示単位（today は unit、週/月は unit_daily 優先）。
    pub fn unit_for(&self, period: &str) -> &str {
        if period == "today" {
            &self.unit
        } else {
            self.unit_daily.as_deref().unwrap_or(&self.unit)
        }
    }
}

/// マシン健全性レポートのコマンド定義（任意）。未設定なら /api/health ごと無効。
/// しきい値判定は下層（embalse）の責務 — mando は exec して契約 JSON を
/// 正規化するだけで、metric 名もコマンド名も本体は知らない（設計原則 2）。
#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    /// バナー表示の対象名（任意。例 "jarvis"）。未指定なら UI は名前なしで出す。
    #[serde(default)]
    pub label: Option<String>,
    /// exec するコマンド配列。
    pub command: Vec<String>,
    /// metric 名 → UI 表示名の置換マップ（任意）。無いキーは metric 名を素通し。
    #[serde(default)]
    pub labels: Option<std::collections::HashMap<String, String>>,
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
    /// open コマンド（shutter 必須 / light・switch 不可）。
    #[serde(default)]
    pub open: Option<Vec<String>>,
    /// close コマンド（shutter 必須 / light・switch 不可）。
    #[serde(default)]
    pub close: Option<Vec<String>>,
    /// stop コマンド（shutter 任意 / light・switch 不可）。
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// on コマンド（light・switch 必須 / shutter 不可）。
    #[serde(default)]
    pub on: Option<Vec<String>>,
    /// off コマンド（light・switch 必須 / shutter 不可）。
    #[serde(default)]
    pub off: Option<Vec<String>>,
    /// 任意色コマンドテンプレ（light のみ・任意）。{color} プレースホルダを
    /// 配列全体でちょうど 1 個含み、検証済み hex（例 "#ff69b4"）に置換して exec される。
    #[serde(default)]
    pub color: Option<Vec<String>>,
    /// 明るさ（調光）コマンドテンプレ（light のみ・任意）。{brightness} プレースホルダを
    /// 配列全体でちょうど 1 個含み、検証済みの整数 1〜100 に置換して exec される。
    #[serde(default)]
    pub brightness: Option<Vec<String>>,
    /// 色・色温度プリセット（light のみ）。
    #[serde(default, rename = "preset")]
    pub presets: Vec<Preset>,
    /// 表示フェイス（switch 専用・任意）。UI のアイコン/セクション/ラベルを切り替える。
    /// 未指定なら素のスイッチ表示。shutter / light では指定不可。
    #[serde(default)]
    pub face: Option<Face>,
    /// このデバイスをグループカードとして描画するときのメンバー device 名(light 専用・任意)。
    /// 一括操作はこのデバイス自身のコマンド(wire group 等)が担い、members は
    /// UI の「個別に操作」展開に使うだけ — mando が members を直列 exec することはない。
    #[serde(default)]
    pub members: Vec<String>,
    /// exec 直列化レーン（任意）。同じ lane のデバイスと直列化される。
    /// 未指定ならデバイス名 = デバイス単位の直列化のみ（他デバイスとは並列）。
    /// echonet 系（enl / casa 経由）は 3610 を専有 bind するため、
    /// 同一の lane（例 "echonet"）を明示すること。
    #[serde(default)]
    pub lane: Option<String>,
}

impl Device {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    /// exec 直列化レーン名。未指定ならデバイス名。
    pub fn exec_lane(&self) -> &str {
        self.lane.as_deref().unwrap_or(&self.name)
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

    pub fn brightness_cmd(&self) -> Option<&[String]> {
        self.brightness.as_deref()
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
    NonShutterInGroup { group: String, member: String },
    DuplicateGroupMember { device: String },
    ColorPlaceholder { device: String, count: usize },
    BrightnessPlaceholder { device: String, count: usize },
    DuplicateGraph(String),
    EmptyGraphQuery(String),
    PeriodPlaceholder { graph: String, count: usize },
    UnknownLightMember { device: String, member: String },
    NonLightMember { device: String, member: String },
    NestedLightMembers { device: String, member: String },
    DuplicateLightMember { member: String },
    EmptyHealthCommand,
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
            ConfigError::NonShutterInGroup { group, member } => {
                write!(f, "group {group}: シャッター以外はグループに入れられない: {member}")
            }
            ConfigError::DuplicateGroupMember { device } => {
                write!(f, "device {device}: 複数のグループに所属できない（UI が個別行をデバイスごとに 1 つしか持てないため）")
            }
            ConfigError::ColorPlaceholder { device, count } => {
                write!(f, "device {device}: color テンプレは {{color}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
            ConfigError::BrightnessPlaceholder { device, count } => {
                write!(f, "device {device}: brightness テンプレは {{brightness}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
            ConfigError::DuplicateGraph(n) => write!(f, "graph 名が重複: {n}"),
            ConfigError::EmptyGraphQuery(n) => write!(f, "graph {n}: query が空"),
            ConfigError::PeriodPlaceholder { graph, count } => {
                write!(f, "graph {graph}: query は {{period}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
            ConfigError::UnknownLightMember { device, member } => {
                write!(f, "device {device}: members が未知の device を参照: {member}")
            }
            ConfigError::NonLightMember { device, member } => {
                write!(f, "device {device}: members に light 以外は入れられない: {member}")
            }
            ConfigError::NestedLightMembers { device, member } => {
                write!(f, "device {device}: member {member} は自身が members を持つため入れ子にできない")
            }
            ConfigError::DuplicateLightMember { member } => {
                write!(f, "device {member}: members に複数回指定されている（同一グループ内の重複または複数グループへの所属）")
            }
            ConfigError::EmptyHealthCommand => write!(f, "health: command が空"),
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
                    forbid(&d.name, "brightness", &d.brightness)?;
                    if d.face.is_some() {
                        return Err(ConfigError::ForbiddenField {
                            device: d.name.clone(),
                            field: "face",
                        });
                    }
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
                    if d.face.is_some() {
                        return Err(ConfigError::ForbiddenField {
                            device: d.name.clone(),
                            field: "face",
                        });
                    }
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
                    if let Some(brightness) = &d.brightness {
                        if brightness.is_empty() {
                            return Err(ConfigError::EmptyCommand(d.name.clone()));
                        }
                        let count: usize = brightness
                            .iter()
                            .map(|s| s.matches("{brightness}").count())
                            .sum();
                        if count != 1 {
                            return Err(ConfigError::BrightnessPlaceholder {
                                device: d.name.clone(),
                                count,
                            });
                        }
                    }
                }
                Kind::Switch => {
                    require(&d.name, "on", &d.on)?;
                    require(&d.name, "off", &d.off)?;
                    forbid(&d.name, "open", &d.open)?;
                    forbid(&d.name, "close", &d.close)?;
                    forbid(&d.name, "stop", &d.stop)?;
                    forbid(&d.name, "color", &d.color)?;
                    forbid(&d.name, "brightness", &d.brightness)?;
                    if !d.presets.is_empty() {
                        return Err(ConfigError::ForbiddenField {
                            device: d.name.clone(),
                            field: "preset",
                        });
                    }
                }
            }
        }

        // light の members(グループカード)検証。親・メンバーとも light 限定、
        // 入れ子と複数親への所属は禁止(UI がタイルをデバイスごとに 1 つしか持てないため)。
        let mut seen_lm = std::collections::HashSet::new();
        for d in &self.devices {
            if d.members.is_empty() {
                continue;
            }
            if d.kind != Kind::Light {
                return Err(ConfigError::ForbiddenField {
                    device: d.name.clone(),
                    field: "members",
                });
            }
            for m in &d.members {
                let Some(md) = self.find(m) else {
                    return Err(ConfigError::UnknownLightMember {
                        device: d.name.clone(),
                        member: m.clone(),
                    });
                };
                if md.kind != Kind::Light {
                    return Err(ConfigError::NonLightMember {
                        device: d.name.clone(),
                        member: m.clone(),
                    });
                }
                if !md.members.is_empty() {
                    return Err(ConfigError::NestedLightMembers {
                        device: d.name.clone(),
                        member: m.clone(),
                    });
                }
                if !seen_lm.insert(m) {
                    return Err(ConfigError::DuplicateLightMember { member: m.clone() });
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
                    // グループは当面シャッター専用（一括開閉の意味論が light/switch に合わない）。
                    Some(d) if d.kind != Kind::Shutter => {
                        return Err(ConfigError::NonShutterInGroup {
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

        let mut seen_gr = std::collections::HashSet::new();
        for g in &self.graphs {
            if !seen_gr.insert(&g.name) {
                return Err(ConfigError::DuplicateGraph(g.name.clone()));
            }
            if g.query.is_empty() {
                return Err(ConfigError::EmptyGraphQuery(g.name.clone()));
            }
            let count: usize = g.query.iter().map(|s| s.matches("{period}").count()).sum();
            if count != 1 {
                return Err(ConfigError::PeriodPlaceholder {
                    graph: g.name.clone(),
                    count,
                });
            }
        }

        if let Some(h) = &self.health {
            if h.command.is_empty() {
                return Err(ConfigError::EmptyHealthCommand);
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

    pub fn find_graph(&self, name: &str) -> Option<&Graph> {
        self.graphs.iter().find(|g| g.name == name)
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
            Err(ConfigError::NonShutterInGroup { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_device_parses() {
        let p = write_tmp(
            "switch",
            r##"
            [[device]]
            name  = "fan"
            alias = "換気扇"
            kind  = "switch"
            get_state = ["casa", "get", "fan", "operation_status"]
            on  = ["casa", "set", "fan", "operation_status", "on"]
            off = ["casa", "set", "fan", "operation_status", "off"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let d = cfg.find("fan").unwrap();
        assert_eq!(d.kind, Kind::Switch);
        assert_eq!(d.label(), "換気扇");
        assert_eq!(d.on_cmd().unwrap()[1], "set");
        assert_eq!(d.off_cmd().unwrap().last().unwrap(), "off");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_requires_on_off() {
        let p = write_tmp(
            "switch_missing",
            r##"
            [[device]]
            name = "fan"
            kind = "switch"
            get_state = ["casa", "get", "fan", "operation_status"]
            on  = ["casa", "set", "fan", "operation_status", "on"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::MissingCommand { field: "off", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_rejects_shutter_and_light_fields() {
        let p = write_tmp(
            "switch_forbidden",
            r##"
            [[device]]
            name = "fan"
            kind = "switch"
            get_state = ["casa", "get", "fan", "operation_status"]
            on  = ["casa", "set", "fan", "operation_status", "on"]
            off = ["casa", "set", "fan", "operation_status", "off"]
            open = ["casa", "set", "fan", "x", "open"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "open", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_in_group_rejected() {
        let p = write_tmp(
            "switchgroup",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[device]]
            name = "fan"
            kind = "switch"
            get_state = ["casa", "get", "fan", "operation_status"]
            on  = ["casa", "set", "fan", "operation_status", "on"]
            off = ["casa", "set", "fan", "operation_status", "off"]
            [[group]]
            name = "all"
            members = ["s1", "fan"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NonShutterInGroup { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_face_light_parses() {
        let p = write_tmp(
            "switch_face",
            r##"
            [[device]]
            name = "fan_light"
            kind = "switch"
            face = "light"
            get_state = ["casa", "get", "fan_light", "power"]
            on  = ["casa", "on",  "fan_light"]
            off = ["casa", "off", "fan_light"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("fan_light").unwrap().face, Some(Face::Light));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_without_face_is_none() {
        let p = write_tmp(
            "switch_noface",
            r##"
            [[device]]
            name = "fan"
            kind = "switch"
            get_state = ["casa", "get", "fan", "power"]
            on  = ["casa", "on",  "fan"]
            off = ["casa", "off", "fan"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("fan").unwrap().face, None);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn shutter_rejects_face() {
        let p = write_tmp(
            "shutter_face",
            r##"
            [[device]]
            name = "s1"
            face = "light"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "face", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_rejects_face() {
        let p = write_tmp(
            "light_face",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            face = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on  = ["mat", "on",  "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "face", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn switch_unknown_face_rejected() {
        let p = write_tmp(
            "switch_badface",
            r##"
            [[device]]
            name = "fan"
            kind = "switch"
            face = "fan"
            get_state = ["casa", "get", "fan", "power"]
            on  = ["casa", "on",  "fan"]
            off = ["casa", "off", "fan"]
            "##,
        );
        assert!(matches!(Config::load(&p), Err(ConfigError::Parse(_))));
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
            brightness: None,
            presets: vec![],
            face: None,
            members: vec![],
            lane: None,
        };
        assert_eq!(d.label(), "x");
        assert!(d.stop_cmd().is_none());
    }

    #[test]
    fn device_lane_defaults_to_device_name() {
        let c: Config = toml::from_str(
            r#"
            [[device]]
            name = "s1"
            get_state = ["true"]
            open = ["true"]
            close = ["true"]

            [[device]]
            name = "s2"
            lane = "echonet"
            get_state = ["true"]
            open = ["true"]
            close = ["true"]
            "#,
        )
        .unwrap();
        assert_eq!(c.devices[0].exec_lane(), "s1");
        assert_eq!(c.devices[1].exec_lane(), "echonet");
    }

    #[test]
    fn exec_timeout_defaults_to_15000() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.exec.timeout_ms, 15_000);

        let c: Config = toml::from_str("[exec]\ntimeout_ms = 3000\n").unwrap();
        assert_eq!(c.exec.timeout_ms, 3_000);
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

    #[test]
    fn brightness_template_parses() {
        let p = write_tmp(
            "brightok",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            brightness = ["mat", "level", "--node", "5", "--percent", "{brightness}"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let d = cfg.find("l1").unwrap();
        assert_eq!(d.brightness_cmd().unwrap().last().unwrap(), "{brightness}");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn brightness_placeholder_zero_rejected() {
        let p = write_tmp(
            "brightzero",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            brightness = ["mat", "level", "--node", "5", "--percent", "50"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::BrightnessPlaceholder { count: 0, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn brightness_placeholder_two_rejected() {
        let p = write_tmp(
            "brighttwo",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            brightness = ["mat", "level", "--node", "5", "--percent", "{brightness}", "--x", "{brightness}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::BrightnessPlaceholder { count: 2, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn brightness_on_shutter_rejected() {
        let p = write_tmp(
            "brightshutter",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            brightness = ["mat", "level", "--node", "5", "--percent", "{brightness}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "brightness", .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn brightness_empty_rejected() {
        let p = write_tmp(
            "brightempty",
            r##"
            [[device]]
            name = "l1"
            kind = "light"
            get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
            on = ["mat", "on", "--node", "5"]
            off = ["mat", "off", "--node", "5"]
            brightness = []
            "##,
        );
        assert!(matches!(Config::load(&p), Err(ConfigError::EmptyCommand(_))));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_parses() {
        let p = write_tmp(
            "graphok",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name       = "generation"
            label      = "太陽光発電"
            unit       = "W"
            unit_daily = "kWh"
            query      = ["embalse-query", "generation", "{period}"]
            [[graph]]
            name  = "co2"
            unit  = "ppm"
            query = ["embalse-query", "co2", "{period}"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let g = cfg.find_graph("generation").unwrap();
        assert_eq!(g.label(), "太陽光発電");
        assert_eq!(g.unit_for("today"), "W");
        assert_eq!(g.unit_for("week"), "kWh");
        assert_eq!(g.unit_for("month"), "kWh");
        let c = cfg.find_graph("co2").unwrap();
        assert_eq!(c.label(), "co2"); // label 未指定は name
        assert_eq!(c.unit_for("week"), "ppm"); // unit_daily 未指定は unit
        assert!(cfg.find_graph("nope").is_none());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_zero_entries_ok() {
        // [[graph]] 無しでも従来どおり起動できる（既存機能への影響ゼロ）。
        let p = write_tmp(
            "graphzero",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert!(cfg.graphs.is_empty());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_duplicate_name_rejected() {
        let p = write_tmp(
            "graphdup",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "g", "{period}"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "g", "{period}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateGraph(_))
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_empty_query_rejected() {
        let p = write_tmp(
            "graphempty",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = []
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::EmptyGraphQuery(_))
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_period_placeholder_zero_rejected() {
        let p = write_tmp(
            "graphp0",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "g", "today"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::PeriodPlaceholder { count: 0, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_period_placeholder_two_rejected() {
        let p = write_tmp(
            "graphp2",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "{period}", "{period}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::PeriodPlaceholder { count: 2, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_series_labels_parses() {
        let p = write_tmp(
            "graphlabels",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name  = "plain"
            unit  = "W"
            query = ["embalse-query", "plain", "{period}"]
            [[graph]]
            name  = "machine"
            label = "jarvis"
            unit  = "%"
            query = ["embalse-query", "machine", "{period}"]
            [graph.series_labels]
            cpu_used_pct = "CPU (%)"
            cpu_temp_c   = "温度 (℃)"
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let g = cfg.find_graph("machine").unwrap();
        let m = g.series_labels.as_ref().unwrap();
        assert_eq!(m.get("cpu_used_pct").unwrap(), "CPU (%)");
        assert_eq!(m.get("cpu_temp_c").unwrap(), "温度 (℃)");
        // series_labels 無しの既存形は None のまま
        assert!(cfg.find_graph("plain").unwrap().series_labels.is_none());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn health_parses() {
        let p = write_tmp(
            "healthok",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [health]
            label   = "jarvis"
            command = ["embalse-query", "health"]
            [health.labels]
            cpu_used_pct = "CPU"
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let h = cfg.health.as_ref().unwrap();
        assert_eq!(h.label.as_deref(), Some("jarvis"));
        assert_eq!(h.command, vec!["embalse-query", "health"]);
        assert_eq!(h.labels.as_ref().unwrap().get("cpu_used_pct").unwrap(), "CPU");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn health_absent_is_none() {
        let p = write_tmp(
            "healthnone",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert!(cfg.health.is_none());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn health_empty_command_rejected() {
        let p = write_tmp(
            "healthempty",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [health]
            command = []
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::EmptyHealthCommand)
        ));
        std::fs::remove_file(p).ok();
    }

    /// members テスト用の light デバイス定義を生成する。
    fn light_toml(name: &str, extra: &str) -> String {
        format!(
            r##"
            [[device]]
            name = "{name}"
            kind = "light"
            get_state = ["mat","read"]
            on  = ["mat","on"]
            off = ["mat","off"]
            {extra}
            "##
        )
    }

    #[test]
    fn light_members_parse() {
        let body = format!(
            "{}{}{}",
            light_toml("parent", r#"members = ["kid1", "kid2"]"#),
            light_toml("kid1", ""),
            light_toml("kid2", ""),
        );
        let p = write_tmp("members_ok", &body);
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("parent").unwrap().members, vec!["kid1", "kid2"]);
        assert!(cfg.find("kid1").unwrap().members.is_empty());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_unknown() {
        let body = light_toml("parent", r#"members = ["ghost"]"#);
        let p = write_tmp("members_unknown", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::UnknownLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_non_light() {
        let body = format!(
            "{}{}",
            light_toml("parent", r#"members = ["sw"]"#),
            r##"
            [[device]]
            name = "sw"
            kind = "switch"
            get_state = ["casa","get"]
            on  = ["casa","on"]
            off = ["casa","off"]
            "##
        );
        let p = write_tmp("members_nonlight", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NonLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_nested() {
        let body = format!(
            "{}{}{}",
            light_toml("grandparent", r#"members = ["parent"]"#),
            light_toml("parent", r#"members = ["kid"]"#),
            light_toml("kid", ""),
        );
        let p = write_tmp("members_nested", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NestedLightMembers { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_duplicate_membership() {
        let body = format!(
            "{}{}{}",
            light_toml("p1", r#"members = ["kid"]"#),
            light_toml("p2", r#"members = ["kid"]"#),
            light_toml("kid", ""),
        );
        let p = write_tmp("members_dup", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_same_parent_duplicate() {
        let body = format!(
            "{}{}",
            light_toml("parent", r#"members = ["kid", "kid"]"#),
            light_toml("kid", ""),
        );
        let p = write_tmp("members_same_parent_dup", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_self_reference() {
        let body = light_toml("parent", r#"members = ["parent"]"#);
        let p = write_tmp("members_self_ref", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NestedLightMembers { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_forbidden_on_shutter() {
        let p = write_tmp(
            "members_shutter",
            r##"
            [[device]]
            name = "s1"
            members = ["s2"]
            get_state = ["enl","get","x","026301","open_close_state"]
            open = ["enl","set","x","026301","open_close_operation","open"]
            close = ["enl","set","x","026301","open_close_operation","close"]
            [[device]]
            name = "s2"
            get_state = ["enl","get","x","026302","open_close_state"]
            open = ["enl","set","x","026302","open_close_operation","open"]
            close = ["enl","set","x","026302","open_close_operation","close"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "members", .. })
        ));
        std::fs::remove_file(p).ok();
    }
}
