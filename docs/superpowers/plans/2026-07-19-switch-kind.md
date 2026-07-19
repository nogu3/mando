# kind = "switch"（enl の on/off 機器）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** enl 経由の on/off 機器（換気扇・床暖房 on/off・エアコン電源など）を新 `kind = "switch"` として mando の UI に追加する。

**Architecture:** 既存の shutter / light の 2 kind に `Switch` を足す。switch は **見た目は light（タイル・タップでトグル）、振る舞いは shutter（set 後に同期で state 再取得＋アクティブ窓ポーリング）**。バックエンドはいずれも既存の kind 分岐に一項足すだけで、新しい exec 系統は作らない。enl の on/off 正規化は `normalize_enl_state` を拡張する一点に閉じる（設計原則4）。

**Tech Stack:** Rust（axum / serde / toml）、素の HTML+JS（`include_str!` で焼き込み）。

## Global Constraints

- **プロトコルを持ち込まない**（設計原則1）: switch も config のコマンド配列を exec するだけ。バイト列・UDP・3610 を mando に入れない。
- **バックエンド非依存**（設計原則2）: enl→casa は config 差し替えのみ。コード変更しない。
- **下層知識は一点に**（設計原則4）: on/off の正規化は `src/normalize.rs` の `normalize_enl_state` の中だけに書く。
- **正直な成否**（設計原則7）: switch は set 後に同期で state を取り直す（light の best-effort ではなく shutter 側の `run_action` に乗る）。
- switch はグループに入れられない（当面シャッター専用の一括開閉意味論に合わない）。
- 検証コマンド: `cargo test` / `cargo clippy -- -D warnings` / `cargo build --release`。
- 既存テストを退行させない。特に `src/normalize.rs` の `picks_named_property_among_many`（`open_close_state` が `operation_status` より優先されること）は緑のまま。

---

## ファイル構成

| ファイル | 責務 | 変更 |
|--|--|--|
| `src/config.rs` | config パース・検証・Kind 定義 | `Kind::Switch`、検証アーム、グループ検証の一般化（`LightInGroup`→`NonShutterInGroup`）、テスト |
| `src/main.rs` | HTTP ルーティング・状態取得 | `fetch_state` の `match device.kind` に Switch アーム |
| `src/normalize.rs` | 下層 JSON → 状態の正規化（一点集約） | `normalize_enl_state` を on/off 対応に拡張、テスト |
| `index.html` | 焼き込み UI | `buildSwitchTile`、switch ラベル、レイアウト分岐、`renderState` の kind 対応 |
| `config.example.toml` | 設定サンプル（ドキュメント） | switch の例を追記 |

タスク順: 1) 型とルーティング（クレートを緑に保つ）→ 2) 正規化 → 3) フロント → 4) サンプル。

---

## Task 1: `Kind::Switch` と検証・ルーティング

`Kind::Switch` を足すと `src/config.rs` の `validate()` と `src/main.rs` の `fetch_state` の
`match device.kind`（どちらも網羅マッチ）がコンパイルエラーになる。両方のアームを本タスクで
同時に足し、クレートを緑に保つ。switch の状態正規化は Task 2 までは Unknown を返すが、
コンパイルは通る（`normalize_enl_state` は既に存在）。

**Files:**
- Modify: `src/config.rs`（Kind enum / ConfigError / Display / validate / group 検証 / 既存テスト）
- Modify: `src/main.rs:202-205`（fetch_state の match）
- Test: `src/config.rs` の `mod tests`

**Interfaces:**
- Consumes: 既存の `require()` / `forbid()` / `ConfigError` / `Kind` / `Device`。
- Produces:
  - `Kind::Switch`（TOML では `kind = "switch"`）。
  - `ConfigError::NonShutterInGroup { group: String, member: String }`（旧 `LightInGroup` を置換）。
  - switch device は `on`/`off`/`get_state` 必須、`open`/`close`/`stop`/`color`/`brightness`/`preset` 禁止。

- [ ] **Step 1: 失敗するテストを書く（config.rs の `mod tests` 末尾付近に追加）**

`src/config.rs` の `mod tests`（`fn light_in_group_rejected` の後など）に以下を追加:

```rust
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
```

そして既存の `light_in_group_rejected` テスト（末尾の `matches!` アーム）を更新:

```rust
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NonShutterInGroup { .. })
        ));
```

- [ ] **Step 2: テストが失敗（コンパイルエラー）することを確認**

Run: `cargo test --lib config 2>&1 | head -30`
Expected: `Kind::Switch` と `ConfigError::NonShutterInGroup` が未定義でコンパイル失敗。

- [ ] **Step 3: `Kind` に `Switch` を追加**

`src/config.rs` の Kind enum（14-17 行付近）:

```rust
pub enum Kind {
    #[default]
    Shutter,
    Light,
    Switch,
}
```

- [ ] **Step 4: `ConfigError::LightInGroup` を `NonShutterInGroup` にリネーム**

`src/config.rs` の enum 定義（227 行付近）:

```rust
    NonShutterInGroup { group: String, member: String },
```

Display 実装（261-263 行付近）:

```rust
            ConfigError::NonShutterInGroup { group, member } => {
                write!(f, "group {group}: シャッター以外はグループに入れられない: {member}")
            }
```

- [ ] **Step 5: グループ検証を「shutter 以外を拒否」に一般化**

`src/config.rs` の group メンバー検証の match（424-430 行付近）:

```rust
                    // グループは当面シャッター専用（一括開閉の意味論が light/switch に合わない）。
                    Some(d) if d.kind != Kind::Shutter => {
                        return Err(ConfigError::NonShutterInGroup {
                            group: g.name.clone(),
                            member: m.clone(),
                        })
                    }
                    Some(_) => {}
```

- [ ] **Step 6: `validate()` に Switch のアームを追加**

`src/config.rs` の `match d.kind`、`Kind::Light => { ... }` アームの直後に追加:

```rust
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
```

- [ ] **Step 7: `main.rs` の `fetch_state` に Switch アームを追加**

`src/main.rs` の `match device.kind`（202-205 行付近）:

```rust
            state: match device.kind {
                Kind::Shutter => normalize_enl_state(&raw),
                Kind::Light => normalize_mat_onoff(&raw),
                Kind::Switch => normalize_enl_state(&raw),
            },
```

- [ ] **Step 8: テストが通ることを確認**

Run: `cargo test --lib 2>&1 | tail -20`
Expected: 全テスト PASS（新 4 テスト + 更新した `light_in_group_rejected` を含む）。

- [ ] **Step 9: clippy とビルド確認**

Run: `cargo clippy -- -D warnings && cargo build 2>&1 | tail -5`
Expected: 警告・エラーなし。

- [ ] **Step 10: コミット**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: kind=switch を追加（enl の on/off 機器・グループ検証を一般化）"
```

---

## Task 2: `normalize_enl_state` を on/off 対応に拡張

switch の get_state は operation_status（電源状態）を読む。`normalize_enl_state` に on/off の
分類を足し、開閉と on/off の両方を同じ enl 正規化関数が受ける（設計原則4）。

**Files:**
- Modify: `src/normalize.rs`（`classify_str` の match / `classify` の数値 match）
- Test: `src/normalize.rs` の `mod tests`

**Interfaces:**
- Consumes: 既存の `normalize_enl_state` / `classify` / `classify_str` / `State`。
- Produces: `normalize_enl_state` が operation_status の on/off を `State::On` / `State::Off` に分類する
  （文字列 `"on"`/`"off"`、EDT 数値 `0x30`/`0x31`、hex 文字列 `"0x30"`/`"30"`、オブジェクト `{"state":"on"}`、casa ラップ各形）。想定外は `State::Unknown`。

- [ ] **Step 1: 失敗するテストを書く（normalize.rs の `mod tests` に追加）**

`src/normalize.rs` の `mod tests` に追加（`casa_envelope_*` テスト群の近く）:

```rust
    #[test]
    fn switch_on_off_object() {
        // enl の実出力: value はオブジェクト {"state": "on"}。
        let raw = json!({"properties":[
            {"epc":"80","name":"operation_status","value":{"state":"on"}}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[
            {"epc":"80","name":"operation_status","value":{"state":"off"}}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
    }

    #[test]
    fn switch_on_off_string() {
        let raw = json!({"properties":[{"name":"operation_status","value":"on"}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[{"name":"operation_status","value":"off"}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
    }

    #[test]
    fn switch_on_off_numeric_edt() {
        // ECHONET Lite operation_status EDT: 0x30=ON / 0x31=OFF。
        let raw = json!({"properties":[{"name":"operation_status","value":0x30}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[{"name":"operation_status","value":0x31}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
    }

    #[test]
    fn switch_on_off_casa_envelope() {
        let raw = json!({
            "device": "fan", "protocol": "echonet",
            "value": {"properties":[
                {"name":"operation_status","value":{"state":"on"}}]}
        });
        assert_eq!(normalize_enl_state(&raw), State::On);
    }

    #[test]
    fn switch_unknown_on_garbage_value() {
        let raw = json!({"properties":[{"name":"operation_status","value":"heating"}]});
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --lib normalize::tests::switch 2>&1 | tail -20`
Expected: FAIL（`"on"`/`0x30` 等が現状 `Unknown` に落ちるため `assertion failed`）。

- [ ] **Step 3: `classify_str` に on/off を追加**

`src/normalize.rs` の `classify_str`（93-102 行付近）の match に、`stopped` の行の後（`_ => State::Unknown` の前）に追加:

```rust
        "on" | "0x30" | "30" => State::On,
        "off" | "0x31" | "31" => State::Off,
```

- [ ] **Step 4: `classify` の数値 match に on/off の EDT を追加**

`src/normalize.rs` の `classify` の `Value::Number` アーム（73-81 行付近）の match に、`0x45` の行の後（`_ => State::Unknown` の前）に追加:

```rust
            Some(0x30) => State::On,
            Some(0x31) => State::Off,
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test --lib normalize 2>&1 | tail -20`
Expected: 全 PASS。特に既存の `picks_named_property_among_many`（`open_close_state` 優先）が緑のままであること。

- [ ] **Step 6: clippy 確認**

Run: `cargo clippy -- -D warnings 2>&1 | tail -5`
Expected: 警告なし。

- [ ] **Step 7: コミット**

```bash
git add src/normalize.rs
git commit -m "feat: normalize_enl_state を operation_status(on/off) 対応に拡張"
```

---

## Task 3: フロント（switch タイル）

switch を light と同じタイル見た目で描画する。ただし振る舞いは shutter 系: 既存の
`kind === "light"` 分岐を素通りするため、同期確認（`deviceAct` の非 light パス）と
ポーリング（`pollOnce` は light のみ除外）に自動的に乗る。既存の tile / bulb CSS を
再利用し（新規 CSS なし）、グリフだけ電源記号（⏻）にする。

**Files:**
- Modify: `index.html`（`buildSwitchTile` 追加 / `SWITCH_LABEL` / `renderState` / `boot()` のレイアウト分岐）

**Interfaces:**
- Consumes: 既存の `cards` Map / `deviceAct` / `renderState` / `sectionHeading` / `STATE_MSG` / `.tile` `.lit` `button.bulb` CSS。
- Produces: `kind === "switch"` のカードオブジェクト（`{kind, rootEl, statusEl, labelEl, msgEl, buttons, state}`）と「🔌 スイッチ」セクション。

- [ ] **Step 1: `SWITCH_LABEL` を追加**

`index.html` の `STATE_LABEL` 定義（361-364 行付近）の直後に追加:

```javascript
const SWITCH_LABEL = { on: "オン", off: "オフ", unknown: "不明" };
```

- [ ] **Step 2: `renderState` を kind 対応にする**

`index.html` の `renderState`（783-794 行付近）の `c.labelEl.textContent = STATE_LABEL[st] || "不明";` の行を次に置換:

```javascript
  const labelMap = c.kind === "switch" ? SWITCH_LABEL : STATE_LABEL;
  c.labelEl.textContent = labelMap[st] || "不明";
```

- [ ] **Step 3: `buildSwitchTile` を追加**

`index.html` の `buildLightTile` 関数（530-568 行付近）の直後に追加:

```javascript
/* ── switch タイル（電源ボタン = on/off トグル。振る舞いは shutter 系）──── */
function buildSwitchTile(dev) {
  const el = document.createElement("div");
  el.className = "tile";
  el.innerHTML = `
    <button class="bulb" type="button" aria-label="オン/オフ">⏻</button>
    <div class="tname"></div>
    <div class="status unknown"><span class="label">不明</span><span class="msg"></span></div>
  `;
  el.querySelector(".tname").textContent = dev.label;
  const c = {
    kind: "switch",
    rootEl: el,
    statusEl: el.querySelector(".status"),
    labelEl: el.querySelector(".status .label"),
    msgEl: el.querySelector(".status .msg"),
    buttons: [...el.querySelectorAll("button")],
    state: "unknown",
  };
  // deviceAct は kind !== "light" なので同期パス（run_action = set 後に state 再取得）を通る。
  el.querySelector(".bulb").addEventListener("click", () =>
    deviceAct(dev.name, c.state === "on" ? "off" : "on")
  );
  cards.set(dev.name, c);
  return el;
}
```

- [ ] **Step 4: `boot()` のレイアウト分岐を更新**

`index.html` の `boot()`（1256-1257 行付近）の `lights`/`shutters` 定義を次に置換:

```javascript
  const lights = devices.filter((d) => d.kind === "light");
  const switches = devices.filter((d) => d.kind === "switch");
  const shutters = devices.filter((d) => d.kind === "shutter");
```

さらに light セクションのブロック（1260-1266 行付近、`if (lights.length) { ... }` の閉じ `}` の直後）に switch セクションを追加:

```javascript
  if (switches.length) {
    app.appendChild(sectionHeading("🔌 スイッチ"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of switches) tiles.appendChild(buildSwitchTile(dev));
    app.appendChild(tiles);
  }
```

- [ ] **Step 5: ビルドが通ることを確認（include_str! で HTML が焼き込まれる）**

Run: `cargo build 2>&1 | tail -5`
Expected: エラーなし（HTML は文字列として焼き込まれるのでコンパイル自体は成功）。

- [ ] **Step 6: switch を含む一時 config で手動起動して目視確認**

一時 config を作成:

```bash
cat > /tmp/mando-switch-test.toml <<'EOF'
bind = "127.0.0.1:8099"
[[device]]
name  = "fan"
alias = "換気扇"
kind  = "switch"
get_state = ["sh", "-c", "echo '{\"properties\":[{\"name\":\"operation_status\",\"value\":{\"state\":\"off\"}}]}'"]
on  = ["sh", "-c", "echo on"]
off = ["sh", "-c", "echo off"]
EOF
```

Run: `MANDO_CONFIG=/tmp/mando-switch-test.toml cargo run 2>/dev/null &` してから `curl -s localhost:8099/api/devices` と `curl -s localhost:8099/api/devices/fan/state` を確認（config パスは環境変数 `MANDO_CONFIG` で渡す。`src/main.rs:49` 参照）。

> 期待: `/api/devices` に `"kind":"switch"` が出る。`/api/devices/fan/state` が `{"state":"off",...}` を返す。

ブラウザ（またはヘッドレス Chromium でのスクリーンショット）で `http://127.0.0.1:8099/` を開き、
「🔌 スイッチ」セクションに換気扇タイルが出て、⏻ をタップすると on/off がトグルし、
set 後の同期確認で状態ラベルが「オン/オフ」に更新されることを確認。確認後サーバを停止。

- [ ] **Step 7: コミット**

```bash
git add index.html
git commit -m "feat: switch タイルを UI に追加（タップで on/off・同期確認＋ポーリング）"
```

---

## Task 4: config.example.toml に switch の例を追記

**Files:**
- Modify: `config.example.toml`

**Interfaces:**
- Consumes: なし（ドキュメントのみ）。
- Produces: `kind = "switch"` のコメント付き例。

- [ ] **Step 1: switch の例を追記**

`config.example.toml` の light の例ブロックの後（グラフセクションの前あたり）に追加:

```toml
# ── ECHONET on/off 機器（換気扇・床暖房 on/off・エアコン電源等。enl/casa 経由）──
# kind = "switch" は on / off / get_state 必須。color・brightness・preset・open/close/stop は不可。
# get_state は operation_status（共通プロパティ EPC 0x80）を読む。値域は機種で振れるため、
# 投入前に `enl describe <IP> <EOJ>` で operation_status の返り形（on/off の表現）を確認する。
# set 後は mando が同期で state を取り直して確定表示する（shutter と同じ正直な確認）。
# [[device]]
# name  = "fan"
# alias = "換気扇"
# kind  = "switch"
# casa 経由（推奨。名前解決は casa の devices.toml が持つ）:
# get_state = ["casa", "get", "fan", "operation_status"]
# on        = ["casa", "set", "fan", "operation_status", "on"]
# off       = ["casa", "set", "fan", "operation_status", "off"]
# enl を直接叩く形（casa を挟まない場合。正規化はこちらも受ける）:
#   get_state = ["enl", "get", "192.0.2.20", "013001", "operation_status"]
#   on        = ["enl", "set", "192.0.2.20", "013001", "operation_status", "on"]
#   off       = ["enl", "set", "192.0.2.20", "013001", "operation_status", "off"]
#
# 注意: switch はグループ（[[group]] members）に入れられない（当面シャッター専用）。
```

- [ ] **Step 2: 例が有効な TOML であることを確認（コメントアウトなので構文チェックのみ）**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: 既存テスト全 PASS（config.example の変更はコメントのみでコードに影響しない）。

- [ ] **Step 3: コミット**

```bash
git add config.example.toml
git commit -m "docs: config.example に kind=switch の例を追記"
```

---

## 完了条件

- `cargo test` / `cargo clippy -- -D warnings` / `cargo build --release` が緑。
- switch を含む config でサーバを起動すると「🔌 スイッチ」セクションにタイルが出る。
- タイルのタップで on↔off がトグルし、set 後の同期確認で状態が確定表示される。
- switch はアクティブ窓の間ポーリングされる（light と違い定期取得される）。
- enl→casa の差し替えが config のコマンド配列だけで済む（コード変更不要）。

## Self-Review メモ

- **Spec coverage:** config.rs（Kind/検証/グループ）=Task1、normalize=Task2、main.rs ルーティング=Task1 Step7、フロント=Task3、config.example=Task4。仕様の全節に対応タスクあり。
- **既存テスト保護:** `picks_named_property_among_many` は `open_close_state` 優先の finder により緑維持（Task2 Step5 で明示確認）。`light_in_group_rejected` は Task1 Step1 で新エラー名に更新。
- **型整合:** `ConfigError::NonShutterInGroup { group, member }`、`Kind::Switch`、カードオブジェクトのキー（`kind/rootEl/statusEl/labelEl/msgEl/buttons/state`）を全タスクで統一。
