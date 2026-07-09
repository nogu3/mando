# ライトデバイス対応（mat 直叩き）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `mat` で制御する Matter ライトを mando の UI から on / off / 色プリセット / kelvin プリセットで操作できるようにする。

**Architecture:** config に `kind = "light"`（省略時 `"shutter"`、既存 config 完全互換）を導入。light は `get_state` / `on` / `off` 必須 + 名前付き `[[device.preset]]`（完成済みコマンド配列）。API は `POST /api/devices/{name}/on|off|presets/{preset}` を追加し、全操作は実行後に state を再取得して確定値を返す（設計原則 7）。正規化は `normalize.rs` に mat read（`{"value": true}`）用を追加し、デバイス kind でディスパッチ。exec（Semaphore(1) 直列化・終了コードマッピング）は無変更 — mat は enl と同じ 0/3/4/5 体系。

**Tech Stack:** Rust / axum 0.7 / serde / toml。テストは cargo test（API テストに tower `util` + http-body-util を dev-dependencies 追加）。UI は index.html（`include_str!` 焼き込み、vanilla JS）。

**Spec:** `docs/superpowers/specs/2026-07-09-light-device-design.md`

## Global Constraints

- 任意値のユーザー入力 → exec 経路を作らない（色・kelvin はプリセット＝config の完成済みコマンド配列のみ）
- 本体コードは backend 非依存のまま（`mat` という文字列をコードに持ち込まない。config のコマンド配列を exec するだけ）
- exec の Semaphore(1) 直列化・ExecOutcome マッピングは変更しない
- ライトは定期ポーリングしない（画面表示時 1 回 + 操作後の state 再取得のみ）。シャッターのポーリングは無変更
- 既存 config（kind 未指定のシャッター）が無変更で動くこと（後方互換）
- 各タスク完了時に `cargo test` 全通過 + `cargo clippy -- -D warnings` クリーン

---

### Task 1: config — kind / on / off / preset とバリデーション

**Files:**
- Modify: `src/config.rs`
- Modify: `src/main.rs:216-222`（`device_cmd` — Option 化した open/close への追随。コンパイルを通すための最小変更のみ）

**Interfaces:**
- Produces（Task 3 が使う）:
  - `pub enum Kind { Shutter, Light }`（`Deserialize + Serialize`, snake_case, `Default = Shutter`）
  - `Device.kind: Kind`
  - `Device::open_cmd() / close_cmd() / on_cmd() / off_cmd() -> Option<&[String]>`（既存 `stop_cmd()` と同形）
  - `Device::preset_cmd(name: &str) -> Option<&[String]>`
  - `Device.presets: Vec<Preset>`、`Preset { name: String, label: Option<String>, cmd: Vec<String> }` + `Preset::label() -> &str`

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `mod tests` に追加（既存の `write_tmp` ヘルパを使う）:

```rust
#[test]
fn light_device_parses() {
    let p = write_tmp(
        "light",
        r#"
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
        cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]
        [[device.preset]]
        name  = "pink"
        cmd   = ["mat", "color", "--node", "5", "--name", "pink"]
        "#,
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
    std::fs::remove_file(p).ok();
}

#[test]
fn default_kind_is_shutter() {
    let p = write_tmp(
        "defkind",
        r#"
        [[device]]
        name = "shutter"
        get_state = ["enl", "get", "x", "026301", "open_close_state"]
        open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
        close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
        "#,
    );
    let cfg = Config::load(&p).unwrap();
    assert_eq!(cfg.find("shutter").unwrap().kind, Kind::Shutter);
    std::fs::remove_file(p).ok();
}

#[test]
fn light_requires_on_off() {
    let p = write_tmp(
        "lightreq",
        r#"
        [[device]]
        name = "l1"
        kind = "light"
        get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
        on = ["mat", "on", "--node", "5"]
        "#,
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
        r#"
        [[device]]
        name = "l1"
        kind = "light"
        get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
        on = ["mat", "on", "--node", "5"]
        off = ["mat", "off", "--node", "5"]
        open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
        "#,
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
        r#"
        [[device]]
        name = "s1"
        get_state = ["enl", "get", "x", "026301", "open_close_state"]
        open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
        close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
        on = ["mat", "on", "--node", "5"]
        "#,
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
        r#"
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
        "#,
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
        r#"
        [[device]]
        name = "l1"
        kind = "light"
        get_state = ["mat", "read", "--node", "5", "-c", "onoff", "-a", "on-off"]
        on = ["mat", "on", "--node", "5"]
        off = ["mat", "off", "--node", "5"]
        [[device.preset]]
        name = "warm"
        cmd = []
        "#,
    );
    assert!(matches!(Config::load(&p), Err(ConfigError::EmptyCommand(_))));
    std::fs::remove_file(p).ok();
}

#[test]
fn light_in_group_rejected() {
    let p = write_tmp(
        "lightgroup",
        r#"
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
        "#,
    );
    assert!(matches!(
        Config::load(&p),
        Err(ConfigError::LightInGroup { .. })
    ));
    std::fs::remove_file(p).ok();
}
```

既存テスト `default_label_is_name` の `Device` リテラルにも新フィールドを足す（Step 3 のコードを参照: `kind: Kind::Shutter, on: None, off: None, presets: vec![]`、`get_state/open/close` は `open: Some(vec!["a".into()])` 形式に変える）。

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test --bin mando config`
Expected: コンパイルエラー（`Kind` / `MissingCommand` 等が未定義）

- [ ] **Step 3: 実装**

`src/config.rs`:

```rust
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
    /// exec するコマンド配列。
    pub cmd: Vec<String>,
}

impl Preset {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}
```

`Device` を変更（open/close を Option 化、kind/on/off/presets を追加）:

```rust
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
}
```

`use serde::{Deserialize, Serialize};` に変更（Serialize は Kind を API レスポンスへ直接出すため）。

`ConfigError` にバリアント追加 + Display:

```rust
MissingCommand { device: String, field: &'static str },
ForbiddenField { device: String, field: &'static str },
DuplicatePreset { device: String, preset: String },
LightInGroup { group: String, member: String },
```

```rust
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
```

既存 `EmptyCommand` の Display 文言を「device {n}: コマンド配列が空」に変える（open/close 必須の説明は MissingCommand 側に移ったため）。

`validate()` を kind 別に書き換え:

```rust
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
            }
        }
    }

    let mut seen_g = std::collections::HashSet::new();
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
        }
    }
    Ok(())
}
```

`validate` の外（モジュール直下）にヘルパ:

```rust
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
```

`src/main.rs` の `device_cmd` を Option 化に追随（挙動は不変。light の open/close/stop は None → 既存の 404 経路に落ちる）:

```rust
fn device_cmd(device: &Device, op: Op) -> Option<Vec<String>> {
    match op {
        Op::Open => device.open_cmd().map(|c| c.to_vec()),
        Op::Close => device.close_cmd().map(|c| c.to_vec()),
        Op::Stop => device.stop_cmd().map(|c| c.to_vec()),
    }
}
```

既存テスト `default_label_is_name` の Device リテラル修正:

```rust
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
    presets: vec![],
};
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS、clippy クリーン

- [ ] **Step 5: コミット**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: config に kind=light と on/off/preset を追加"
```

---

### Task 2: normalize — State::On/Off と mat onoff 正規化

**Files:**
- Modify: `src/normalize.rs`

**Interfaces:**
- Consumes: なし（独立）
- Produces（Task 3 が使う）:
  - `State::On` / `State::Off`（serde で `"on"` / `"off"`）
  - `pub fn normalize_mat_onoff(raw: &Value) -> State`

- [ ] **Step 1: 失敗するテストを書く**

`src/normalize.rs` の `mod tests` に追加:

```rust
#[test]
fn mat_onoff_real_format() {
    // mat read の実出力形式。
    let raw = json!({
        "timestamp": "2026-07-09T12:00:00+09:00",
        "node_id": 5, "endpoint": 1,
        "cluster": "onoff", "attribute": "on-off",
        "value": true
    });
    assert_eq!(normalize_mat_onoff(&raw), State::On);
    let raw = json!({"value": false});
    assert_eq!(normalize_mat_onoff(&raw), State::Off);
}

#[test]
fn mat_onoff_garbage_is_unknown() {
    assert_eq!(normalize_mat_onoff(&json!({})), State::Unknown);
    assert_eq!(normalize_mat_onoff(&json!({"value": "on"})), State::Unknown);
    assert_eq!(normalize_mat_onoff(&json!({"value": 1})), State::Unknown);
    assert_eq!(normalize_mat_onoff(&json!(null)), State::Unknown);
}

#[test]
fn on_off_serialize_snake_case() {
    assert_eq!(serde_json::to_string(&State::On).unwrap(), "\"on\"");
    assert_eq!(serde_json::to_string(&State::Off).unwrap(), "\"off\"");
}
```

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test --bin mando normalize`
Expected: コンパイルエラー（`On` / `normalize_mat_onoff` 未定義）

- [ ] **Step 3: 実装**

`State` にバリアント追加:

```rust
    /// light 点灯（mat onoff value=true）。
    On,
    /// light 消灯（mat onoff value=false）。
    Off,
```

正規化関数を追加（enl 正規化と同じく、下層固有知識はこのファイルに閉じる）:

```rust
/// mat read（onoff / on-off）の出力 JSON を正規化する。
///
/// mat の実出力例:
/// `{"timestamp":"...","node_id":5,"endpoint":1,"cluster":"onoff",
///   "attribute":"on-off","value":true}`
/// → `value` の bool で点灯/消灯を判定する。スキーマや値が想定外なら Unknown。
/// casa 移行時はこの関数の中身だけ差し替える（設計原則 4）。
pub fn normalize_mat_onoff(raw: &Value) -> State {
    match raw.get("value") {
        Some(Value::Bool(true)) => State::On,
        Some(Value::Bool(false)) => State::Off,
        _ => State::Unknown,
    }
}
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS、clippy クリーン

- [ ] **Step 5: コミット**

```bash
git add src/normalize.rs
git commit -m "feat: mat onoff の状態正規化（on/off）を追加"
```

---

### Task 3: API — on / off / preset エンドポイントと kind ディスパッチ

**Files:**
- Modify: `src/main.rs`
- Modify: `Cargo.toml`（dev-dependencies 追加）

**Interfaces:**
- Consumes: Task 1 の `Kind` / `on_cmd()` / `off_cmd()` / `preset_cmd()` / `presets`、Task 2 の `State::On/Off` / `normalize_mat_onoff`
- Produces（Task 4 の UI が叩く）:
  - `GET /api/devices` の各要素に `kind: "shutter"|"light"` と `presets: [{name, label}]`
  - `POST /api/devices/{name}/on` / `off` / `presets/{preset}` → 既存 ActionView 形（`{action, state, exec, raw}`）
  - kind 不整合の操作・未知 preset は 404 + `{"error": ...}`

- [ ] **Step 1: dev-dependencies を追加**

`Cargo.toml` に追記:

```toml
[dev-dependencies]
tower = { version = "0.4", features = ["util"] }
http-body-util = "0.1"
```

- [ ] **Step 2: 失敗するテストを書く**

`src/main.rs` 末尾に追加。前提: router 構築を `fn router(app: Shared) -> Router` に抽出してテストから oneshot で叩く（Step 4 で実装）:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// sh で下層 CLI を偽装したテスト用 App。
    /// get_state は mat read / enl の実出力形式を printf で返す。
    fn test_app() -> Shared {
        let cfg: Config = toml::from_str(
            r#"
            [[device]]
            name = "light"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            cmd   = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "shutter"
            get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
            open  = ["sh", "-c", "printf '{}'"]
            close = ["sh", "-c", "printf '{}'"]
            "#,
        )
        .unwrap();
        Arc::new(App {
            config: cfg,
            executor: Executor::new(),
        })
    }

    async fn call(method: &str, path: &str) -> (axum::http::StatusCode, Value) {
        let res = router(test_app())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn devices_list_has_kind_and_presets() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let light = arr.iter().find(|d| d["name"] == "light").unwrap();
        assert_eq!(light["kind"], "light");
        assert_eq!(light["presets"][0]["name"], "warm");
        assert_eq!(light["presets"][0]["label"], "電球色");
        let sh = arr.iter().find(|d| d["name"] == "shutter").unwrap();
        assert_eq!(sh["kind"], "shutter");
        assert_eq!(sh["presets"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn light_state_normalized_as_on() {
        let (st, v) = call("GET", "/api/devices/light/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "on");
    }

    #[tokio::test]
    async fn shutter_state_still_normalized_as_open() {
        let (st, v) = call("GET", "/api/devices/shutter/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "open");
    }

    #[tokio::test]
    async fn light_on_returns_confirmed_state() {
        let (st, v) = call("POST", "/api/devices/light/on").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // 楽観表示ではなく再取得した確定値。
        assert_eq!(v["state"], "on");
    }

    #[tokio::test]
    async fn preset_runs_and_confirms_state() {
        let (st, v) = call("POST", "/api/devices/light/presets/warm").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        assert_eq!(v["state"], "on");
    }

    #[tokio::test]
    async fn kind_mismatch_is_404() {
        let (st, _) = call("POST", "/api/devices/light/open").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call("POST", "/api/devices/light/stop").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call("POST", "/api/devices/shutter/on").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call("POST", "/api/devices/shutter/presets/warm").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_preset_is_404() {
        let (st, v) = call("POST", "/api/devices/light/presets/nope").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(v["error"], "unknown preset");
    }

    #[tokio::test]
    async fn unknown_device_is_404() {
        let (st, _) = call("POST", "/api/devices/ghost/on").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
}
```

- [ ] **Step 3: テストが落ちることを確認**

Run: `cargo test --bin mando`
Expected: コンパイルエラー（`router` 関数未定義、`on_device` 等未定義）

- [ ] **Step 4: 実装**

`src/main.rs`:

use を更新:

```rust
use config::{Config, Device, Kind};
use normalize::{normalize_enl_state, normalize_mat_onoff, State as DeviceState};
```

router 構築を関数に抽出し、`main()` は `let router = router(app);` に置き換え。新ルート 3 本を追加:

```rust
/// 安定ミニ API のルーティング（テストからも oneshot で叩く）。
fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/devices", get(list_devices))
        .route("/api/devices/:name/state", get(get_state))
        .route("/api/devices/:name/open", post(open_device))
        .route("/api/devices/:name/close", post(close_device))
        .route("/api/devices/:name/stop", post(stop_device))
        .route("/api/devices/:name/on", post(on_device))
        .route("/api/devices/:name/off", post(off_device))
        .route("/api/devices/:name/presets/:preset", post(preset_device))
        .route("/api/groups", get(list_groups))
        .route("/api/groups/:name/open", post(group_open))
        .route("/api/groups/:name/close", post(group_close))
        .route("/api/groups/:name/stop", post(group_stop))
        .with_state(app)
}
```

`DeviceInfo` を拡張:

```rust
#[derive(Serialize)]
struct PresetInfo {
    name: String,
    label: String,
}

#[derive(Serialize)]
struct DeviceInfo {
    name: String,
    label: String,
    kind: Kind,
    /// stop 操作に対応しているか（UI が停止ボタンを出すか判断する）。
    stop: bool,
    /// light のプリセット（shutter は空）。
    presets: Vec<PresetInfo>,
}
```

```rust
async fn list_devices(State(app): State<Shared>) -> Json<Vec<DeviceInfo>> {
    let devices = app
        .config
        .devices
        .iter()
        .map(|d| DeviceInfo {
            name: d.name.clone(),
            label: d.label().to_string(),
            kind: d.kind,
            stop: d.stop_cmd().is_some(),
            presets: d
                .presets
                .iter()
                .map(|p| PresetInfo {
                    name: p.name.clone(),
                    label: p.label().to_string(),
                })
                .collect(),
        })
        .collect();
    Json(devices)
}
```

`fetch_state` の正規化を kind でディスパッチ（`normalize_enl_state(&raw)` の行を置換）:

```rust
        Ok(raw) => StateView {
            state: match device.kind {
                Kind::Shutter => normalize_enl_state(&raw),
                Kind::Light => normalize_mat_onoff(&raw),
            },
            exec: result.outcome,
            raw: Some(raw),
        },
```

`Op` に On / Off を追加し、`device_cmd` を拡張:

```rust
/// 操作の種類。
#[derive(Clone, Copy)]
enum Op {
    Open,
    Close,
    Stop,
    On,
    Off,
}

/// device の該当操作コマンドを返す。kind が対応しない操作は None。
fn device_cmd(device: &Device, op: Op) -> Option<Vec<String>> {
    match op {
        Op::Open => device.open_cmd().map(|c| c.to_vec()),
        Op::Close => device.close_cmd().map(|c| c.to_vec()),
        Op::Stop => device.stop_cmd().map(|c| c.to_vec()),
        Op::On => device.on_cmd().map(|c| c.to_vec()),
        Op::Off => device.off_cmd().map(|c| c.to_vec()),
    }
}
```

ハンドラ追加:

```rust
async fn on_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::On).await
}

async fn off_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::Off).await
}

/// 名前付きプリセット exec → state 再取得（設計原則 7）。
async fn preset_device(
    State(app): State<Shared>,
    Path((name, preset)): Path<(String, String)>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    match device.preset_cmd(&preset) {
        Some(cmd) => Json(run_action(&app, device, cmd).await).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unknown preset\",\"name\":{},\"preset\":{}}}",
                json_str(&name),
                json_str(&preset)
            ),
        )
            .into_response(),
    }
}
```

`device_op` の 404 文言を一般化（stop 以外にも kind 不整合で使うため）:

```rust
        // この kind では対応しない操作。
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unsupported operation\",\"name\":{}}}",
                json_str(name)
            ),
        )
            .into_response(),
```

- [ ] **Step 5: テスト全通過を確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS（新規 API テスト含む）、clippy クリーン

- [ ] **Step 6: コミット**

```bash
git add src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: API に on/off/preset エンドポイントと kind ディスパッチを追加"
```

---

### Task 4: UI — light カード（つける/消す + プリセットチップ、ポーリングなし）

**Files:**
- Modify: `index.html`

**Interfaces:**
- Consumes: Task 3 の `GET /api/devices`（`kind` / `presets`）、`POST /api/devices/{name}/on|off|presets/{preset}`、state 値 `"on"|"off"`

- [ ] **Step 1: CSS 追加**

`index.html` の `<style>` 内、`button.stop { ... }` の行の直後に追加:

```css
  button.on  { background: linear-gradient(160deg, var(--open),   var(--open2)); }
  button.off { background: linear-gradient(160deg, var(--stop),   var(--stop2)); }
```

`.status.stopped .dot { ... }` の行の直後に追加:

```css
  .status.on  .dot { background: var(--open);   box-shadow: 0 0 0 3px rgba(24,184,138,.16), 0 0 10px rgba(24,184,138,.5); }
  .status.off .dot { background: var(--unknown); }
```

`.controls { ... }` ブロックの直前にプリセットチップの CSS を追加:

```css
  /* ── light プリセットチップ ─────────────────────── */
  .presets { display: flex; flex-wrap: wrap; gap: 7px; margin-top: 9px; }
  .presets:empty { display: none; }
  button.chip {
    appearance: none; border: 1px solid var(--line2); border-radius: 999px;
    background: rgba(255,255,255,.06); color: var(--fg);
    padding: 8px 14px; font-size: 13px; font-weight: 600; cursor: pointer;
    min-height: 38px; touch-action: manipulation; user-select: none;
    transition: transform var(--tap), opacity var(--tap), background var(--tap);
  }
  button.chip:active:not(:disabled) { transform: scale(.95); background: rgba(255,255,255,.12); }
  button.chip:disabled { opacity: .4; cursor: progress; }
```

- [ ] **Step 2: JS の定数とラベルを拡張**

`STATE_LABEL` / `VERB` を更新:

```js
const STATE_LABEL = {
  open: "開", closed: "閉", opening: "開いています…", closing: "閉じています…",
  stopped: "途中で停止", on: "点灯", off: "消灯", unknown: "不明",
};
const VERB = {
  open: "開けています…", close: "閉めています…", stop: "止めています…",
  on: "つけています…", off: "消しています…", preset: "変更中…",
};
```

- [ ] **Step 3: light カードのビルダーを追加し、boot で分岐**

`buildCard` 関数の直後に追加:

```js
/* ── light カード（つける/消す + プリセットチップ）── */
function buildLightCard(dev) {
  const el = document.createElement("div");
  el.className = "device";
  el.innerHTML = `
    <div class="info">
      <div class="name"></div>
      <div class="status unknown"><span class="dot"></span><span class="label">不明</span><span class="msg"></span></div>
      <div class="presets"></div>
    </div>
    <div class="controls">
      <button class="act on" data-op="on">点</button>
      <button class="act off" data-op="off">消</button>
    </div>
  `;
  el.querySelector(".name").textContent = dev.label;
  const pr = el.querySelector(".presets");
  for (const p of dev.presets) {
    const b = document.createElement("button");
    b.className = "chip";
    b.textContent = p.label;
    b.addEventListener("click", () => presetAct(dev.name, p.name));
    pr.appendChild(b);
  }
  const c = {
    kind: "light",
    statusEl: el.querySelector(".status"),
    labelEl: el.querySelector(".status .label"),
    msgEl: el.querySelector(".status .msg"),
    buttons: [...el.querySelectorAll("button")], // act + chip 全部を busy 対象に
    state: "unknown",
  };
  for (const btn of el.querySelectorAll("button.act")) {
    btn.addEventListener("click", () => deviceAct(dev.name, btn.dataset.op));
  }
  cards.set(dev.name, c);
  return el;
}

/* ── プリセット実行 ─────────────────────────────── */
async function presetAct(name, preset) {
  busyCount++;
  setDeviceBusy(name, true);
  const c = cards.get(name);
  if (c) c.msgEl.textContent = VERB.preset;
  try {
    const view = await api(
      "POST",
      `/api/devices/${encodeURIComponent(name)}/presets/${encodeURIComponent(preset)}`
    );
    renderState(name, view);
    const am = ACTION_MSG[view.action] || "";
    if (am) { c.msgEl.textContent = "⚠ " + am; c.statusEl.classList.add("error"); }
  } catch (e) {
    if (c) { c.msgEl.textContent = "⚠ 通信エラー"; c.statusEl.classList.add("error"); }
  } finally {
    setDeviceBusy(name, false);
    busyCount--;
  }
}
```

`buildCard` 側にも `kind: "shutter"` をカード record に追加（`state: "unknown"` の前の行に `kind: "shutter",`）。

`boot()` のカード生成ループを分岐に変更:

```js
  for (const dev of devices) {
    app.appendChild(dev.kind === "light" ? buildLightCard(dev) : buildCard(dev));
  }
```

- [ ] **Step 4: light をポーリング対象から外し、表示時に 1 回だけ取得**

`pollOnce()` のループ先頭に kind ガードを追加:

```js
async function pollOnce() {
  if (busyCount > 0) return;
  for (const [name, c] of cards) {
    if (busyCount > 0) return;
    // light は定期ポーリングしない（mat 直叩きは遅く、exec 直列を詰まらせる）。
    // 表示時 1 回 + 操作後の再取得のみ（fetchLightStatesOnce / deviceAct）。
    if (c.kind === "light") continue;
    try {
      const view = await api("GET", `/api/devices/${encodeURIComponent(name)}/state`);
      renderState(name, view);
    } catch (e) {
      if (c) { c.msgEl.textContent = "接続なし"; c.statusEl.classList.add("error"); }
    }
  }
  updateGroupSummaries();
}
```

`boot()` の `startPolling();` の直後に 1 回だけの light state 取得を追加:

```js
  fetchLightStatesOnce(devices);
```

`startPolling` の直後に関数を追加:

```js
/* ── light の初回 state 取得（1 回だけ・ポーリングなし）── */
async function fetchLightStatesOnce(devices) {
  for (const dev of devices) {
    if (dev.kind !== "light") continue;
    try {
      const view = await api("GET", `/api/devices/${encodeURIComponent(dev.name)}/state`);
      renderState(dev.name, view);
    } catch (e) {
      const c = cards.get(dev.name);
      if (c) { c.msgEl.textContent = "接続なし"; c.statusEl.classList.add("error"); }
    }
  }
}
```

`visibilitychange` は `pollOnce` のままで良い（light は再取得されない = 仕様どおり）。

- [ ] **Step 5: fake config で end-to-end 動作確認**

テスト用 config を作って起動し、curl で確認（`sh -c` で mat を偽装。sleep で「遅い mat」も再現）:

```bash
cat > /tmp/mando_light_test.toml <<'EOF'
bind = "127.0.0.1:18099"

[[device]]
name = "shutter"
alias = "シャッター"
get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
open  = ["sh", "-c", "printf '{}'"]
close = ["sh", "-c", "printf '{}'"]

[[device]]
name = "living_lights"
alias = "リビング照明"
kind = "light"
get_state = ["sh", "-c", "sleep 0.3; printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]

[[device.preset]]
name  = "warm"
label = "電球色"
cmd   = ["sh", "-c", "printf '{}'"]

[[device.preset]]
name  = "pink"
label = "ピンク"
cmd   = ["sh", "-c", "printf '{}'"]
EOF
MANDO_CONFIG=/tmp/mando_light_test.toml cargo run &
sleep 2
curl -s http://127.0.0.1:18099/api/devices | jq .
curl -s -X POST http://127.0.0.1:18099/api/devices/living_lights/on | jq .
curl -s -X POST http://127.0.0.1:18099/api/devices/living_lights/presets/warm | jq .
curl -s -X POST http://127.0.0.1:18099/api/devices/living_lights/presets/nope -o /dev/null -w '%{http_code}\n'
curl -s http://127.0.0.1:18099/ | grep -c buildLightCard
kill %1
```

Expected:
- `/api/devices` に living_lights が `kind: "light"`、presets 2 件で出る
- `on` は `{"action":"success","state":"on",...}`
- `presets/warm` も `state: "on"` を返す
- `presets/nope` は 404
- `/` の HTML に `buildLightCard` が含まれる（焼き込み反映。※ `cargo run` はソースから再ビルドするので反映される）

- [ ] **Step 6: テストと clippy を再確認しコミット**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS

```bash
git add index.html
git commit -m "feat: light カード UI（つける/消す + プリセットチップ）"
```

---

### Task 5: ドキュメント — config.example.toml と README

**Files:**
- Modify: `config.example.toml`
- Modify: `README.md:23-27`（API 一覧）

**Interfaces:**
- Consumes: Task 1 の config 形、Task 3 の API 形

- [ ] **Step 1: config.example.toml に light の例を追加**

ファイル末尾に追加:

```toml
# ── Matter ライト（mat 直叩き）─────────────────────────
# kind = "light" は on / off / get_state 必須。色・色温度は [[device.preset]] に
# 完成済みコマンドを並べる（UI にチップボタンが出る。任意値入力は作らない）。
# node 番号は `mat discover` で確認。将来 casa に差し替えるときも配列の差し替えのみ。
# [[device]]
# name  = "living_lights"
# alias = "リビング照明"
# kind  = "light"
# get_state = ["mat", "read", "--node", "5", "--cluster", "onoff", "--attribute", "on-off"]
# on    = ["mat", "on",  "--node", "5"]
# off   = ["mat", "off", "--node", "5"]
#
# [[device.preset]]
# name  = "warm"
# label = "電球色"
# cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]
#
# [[device.preset]]
# name  = "daylight"
# label = "白色"
# cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "5000"]
#
# [[device.preset]]
# name  = "pink"
# label = "ピンク"
# cmd   = ["mat", "color", "--node", "5", "--name", "pink"]
#
# 注意: light はグループ（[[group]] members）に入れられない（当面シャッター専用）。
# 注意: light は定期ポーリングしない（mat 直叩きは 1 コール数秒 + exec 直列のため）。
#       表示時 1 回 + 操作後の確定表示のみ。
```

- [ ] **Step 2: README の API 一覧に追記**

`README.md` の `POST /api/devices/{name}/close` の行の後に追加:

```markdown
- `POST /api/devices/{name}/on` — light を点灯 → **直後に state 再取得**（`state: "on|off|unknown"`）
- `POST /api/devices/{name}/off` — 同上（消灯）
- `POST /api/devices/{name}/presets/{preset}` — config の名前付きプリセット（色・色温度）を実行 → state 再取得
```

- [ ] **Step 3: コミット**

```bash
git add config.example.toml README.md
git commit -m "docs: light デバイスの config 例と API を追記"
```
