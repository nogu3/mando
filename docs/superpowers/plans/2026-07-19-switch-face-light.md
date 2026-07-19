# switch の face = "light" 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `kind = "switch"` の device が任意フィールド `face = "light"` を持てるようにし、指定された switch を 💡 アイコン・「💡 照明」セクション・「点灯/消灯」ラベルで表示する（振る舞いは switch のまま）。

**Architecture:** kind は switch のまま、表示の面（アイコン/セクション/ラベル）だけを config の 1 フィールドで切り替える。backend は `face` を parse・検証して `/api/devices` に載せるだけ。表示判断はフロントが `dev.face` を見て行う（設計原則3）。

**Tech Stack:** Rust（serde / toml / axum）、素の HTML+JS（`include_str!` 焼き込み）。

## Global Constraints

- **表示は config 駆動・フロント隔離（原則3）**: フロントは `kind` と `face` と on/off/unknown しか見ない。EPC・enl・casa を知らない。
- **face は表示専用**: 正規化・操作ルーティング・ポーリング・同期確認（switch の挙動）は一切変えない。
- `face` は `kind = "switch"` 専用。shutter / light に書いたら `ForbiddenField { field: "face" }`。値は `"light"` のみ（未知値は parse エラー）。
- 新規 CSS を足さない（既存 `.tile`/`.lit`/`button.bulb` を再利用）。
- 検証コマンド: `cargo test --bin mando` / `cargo clippy -- -D warnings`（bin-only crate なので `--lib` は使わない）。
- 既存テスト・既存挙動（shutter / light / 素の switch）を退行させない。

---

## ファイル構成

| ファイル | 責務 | 変更 |
|--|--|--|
| `src/config.rs` | Face 型・Device.face・検証 | `Face` enum、`Device.face`、shutter/light アームで face 禁止、テスト |
| `src/main.rs` | `/api/devices` 出力 | `DeviceInfo.face` を公開、`Face` を import |
| `index.html` | 焼き込み UI | `buildSwitchTile` グリフ出し分け、`renderState` ラベル face 対応、`boot()` レイアウト分岐 |
| `config.example.toml` | 設定サンプル | switch 例に `face` 説明追記 |

タスク順: 1) backend（型・検証・API）→ 2) フロント → 3) サンプル。jarvis 実機への `face` 付与は本計画外（マージ後の運用ステップ）。

---

## Task 1: `Face` 型・`Device.face`・検証・API 公開

**Files:**
- Modify: `src/config.rs`（Face enum / Device.face / validate の Shutter・Light アーム / テスト）
- Modify: `src/main.rs`（import に Face 追加 / DeviceInfo.face / list_devices）
- Test: `src/config.rs` の `mod tests`

**Interfaces:**
- Produces:
  - `pub enum Face { Light }`（TOML では `face = "light"`）。`Copy + PartialEq + Deserialize + Serialize`。
  - `Device.face: Option<Face>`（`#[serde(default)]`）。
  - `/api/devices` の各要素に `face`（`"light"` または `null`）。

- [ ] **Step 1: 失敗するテストを書く（config.rs の `mod tests` に追加）**

`src/config.rs` の `mod tests`（`switch_in_group_rejected` などの近く）に追加:

```rust
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
```

- [ ] **Step 2: テストが失敗（コンパイルエラー）することを確認**

Run: `cargo test --bin mando switch_face 2>&1 | head -20`
Expected: `Face` / `Device.face` が未定義でコンパイル失敗。

- [ ] **Step 3: `Face` enum を追加**

`src/config.rs` の `Kind` enum 定義（`pub enum Kind { ... }`）の直後に追加:

```rust
/// switch の表示フェイス（表示専用。振る舞いは switch のまま）。UI のアイコン・
/// セクション・ラベルを切り替えるだけで、正規化や操作には影響しない。
// derive は既存 Kind と同じ Copy + PartialEq + Eq + Deserialize + Serialize。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Face {
    /// 💡 として「💡 照明」セクションに並べ、状態は「点灯/消灯」で出す。
    Light,
}
```

- [ ] **Step 4: `Device` に `face` フィールドを追加**

`src/config.rs` の `Device` 構造体の `presets` フィールド（`pub presets: Vec<Preset>,`）の直後に追加:

```rust
    /// 表示フェイス（switch 専用・任意）。UI のアイコン/セクション/ラベルを切り替える。
    /// 未指定なら素のスイッチ表示。shutter / light では指定不可。
    #[serde(default)]
    pub face: Option<Face>,
```

- [ ] **Step 5: shutter / light アームで `face` を禁止**

`src/config.rs` の `validate()` の `Kind::Shutter` アーム内、`forbid(&d.name, "brightness", &d.brightness)?;` の直後に追加:

```rust
                    if d.face.is_some() {
                        return Err(ConfigError::ForbiddenField {
                            device: d.name.clone(),
                            field: "face",
                        });
                    }
```

同じブロックを `Kind::Light` アーム内の先頭付近（`forbid(&d.name, "stop", &d.stop)?;` の直後）にも追加:

```rust
                    if d.face.is_some() {
                        return Err(ConfigError::ForbiddenField {
                            device: d.name.clone(),
                            field: "face",
                        });
                    }
```

（`Kind::Switch` アームには追加しない = face 許可。）

- [ ] **Step 6: テストが通ることを確認**

Run: `cargo test --bin mando 2>&1 | tail -5`
Expected: 全 PASS（新 5 テスト含む）。

- [ ] **Step 7: `main.rs` の `DeviceInfo` に `face` を公開**

`src/main.rs` の import 行 `use config::{Config, Device, Kind};` を次に変更:

```rust
use config::{Config, Device, Face, Kind};
```

`DeviceInfo` 構造体の `presets: Vec<PresetInfo>,` の直後に追加:

```rust
    /// switch の表示フェイス（表示専用。null なら素のスイッチ）。
    face: Option<Face>,
```

`list_devices` の `DeviceInfo { ... }` 生成内、`presets: d.presets.iter()...collect(),` の直後に追加:

```rust
            face: d.face,
```

- [ ] **Step 8: ビルド・テスト・clippy 確認**

Run: `cargo test --bin mando 2>&1 | tail -3 && cargo clippy -- -D warnings 2>&1 | tail -2`
Expected: 全テスト PASS、警告なし。

- [ ] **Step 9: コミット**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: switch に face=light を追加（表示フェイス。API に公開）"
```

---

## Task 2: フロント（💡 照明としての表示）

**Files:**
- Modify: `index.html`（`buildSwitchTile` / `renderState` / `boot()`）

**Interfaces:**
- Consumes: `/api/devices` の各要素の `face`（`"light"` または `null`）。既存の `cards` / `deviceAct` / `sectionHeading` / `buildLightTile` / `.tile`/`.lit`/`button.bulb` CSS。

- [ ] **Step 1: `buildSwitchTile` を face 対応にする**

`index.html` の `buildSwitchTile` 関数を次に置換（グリフ・aria-label を face で出し分け、`c.face` を保存）:

```javascript
/* ── switch タイル（電源ボタン = on/off トグル。振る舞いは shutter 系）──── */
function buildSwitchTile(dev) {
  const el = document.createElement("div");
  el.className = "tile";
  const isLight = dev.face === "light";
  el.innerHTML = `
    <button class="bulb" type="button" aria-label="${isLight ? "点灯/消灯" : "オン/オフ"}">${isLight ? "💡" : "⏻"}</button>
    <div class="tname"></div>
    <div class="status unknown"><span class="label">不明</span><span class="msg"></span></div>
  `;
  el.querySelector(".tname").textContent = dev.label;
  const c = {
    kind: "switch",
    face: dev.face || null, // "light" のとき点灯/消灯ラベル・💡
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

- [ ] **Step 2: `renderState` のラベル選択を face 対応にする**

`index.html` の `renderState` 内の次の 1 行:

```javascript
  const labelMap = c.kind === "switch" ? SWITCH_LABEL : STATE_LABEL;
```

を次に置換（light-faced switch は STATE_LABEL = 点灯/消灯 を使う）:

```javascript
  const labelMap = (c.kind === "switch" && c.face !== "light") ? SWITCH_LABEL : STATE_LABEL;
```

- [ ] **Step 3: `boot()` のレイアウト分岐を更新**

`index.html` の `boot()` 内、次のブロック:

```javascript
  const lights = devices.filter((d) => d.kind === "light");
  const switches = devices.filter((d) => d.kind === "switch");
  const shutters = devices.filter((d) => d.kind === "shutter");
  const grouped = new Set(grps.flatMap((g) => g.members));

  if (lights.length) {
    app.appendChild(sectionHeading("💡 照明"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of lights) tiles.appendChild(buildLightTile(dev));
    app.appendChild(tiles);
  }
  if (switches.length) {
    app.appendChild(sectionHeading("🔌 スイッチ"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of switches) tiles.appendChild(buildSwitchTile(dev));
    app.appendChild(tiles);
  }
```

を次に置換（light-faced switch を照明セクションへ合流、素の switch のみスイッチセクション）:

```javascript
  const lights = devices.filter((d) => d.kind === "light");
  const switchLights = devices.filter((d) => d.kind === "switch" && d.face === "light");
  const plainSwitches = devices.filter((d) => d.kind === "switch" && d.face !== "light");
  const shutters = devices.filter((d) => d.kind === "shutter");
  const grouped = new Set(grps.flatMap((g) => g.members));

  if (lights.length || switchLights.length) {
    app.appendChild(sectionHeading("💡 照明"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of lights) tiles.appendChild(buildLightTile(dev));
    for (const dev of switchLights) tiles.appendChild(buildSwitchTile(dev));
    app.appendChild(tiles);
  }
  if (plainSwitches.length) {
    app.appendChild(sectionHeading("🔌 スイッチ"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of plainSwitches) tiles.appendChild(buildSwitchTile(dev));
    app.appendChild(tiles);
  }
```

- [ ] **Step 4: ビルド確認（HTML は include_str! で焼き込み）**

Run: `cargo build 2>&1 | tail -3`
Expected: エラーなし。

- [ ] **Step 5: switch(face=light) + 素の switch を含む一時 config で目視/curl 確認**

一時 config を作成:

```bash
cat > /tmp/mando-face-test.toml <<'EOF'
bind = "127.0.0.1:8092"
[[device]]
name  = "kitchen"
alias = "キッチン照明"
kind  = "switch"
face  = "light"
get_state = ["sh", "-c", "echo '{\"properties\":[{\"name\":\"power\",\"value\":{\"power\":\"on\"}}]}'"]
on  = ["sh", "-c", "echo on"]
off = ["sh", "-c", "echo off"]
[[device]]
name  = "fan"
alias = "換気扇"
kind  = "switch"
get_state = ["sh", "-c", "echo '{\"properties\":[{\"name\":\"power\",\"value\":{\"power\":\"off\"}}]}'"]
on  = ["sh", "-c", "echo on"]
off = ["sh", "-c", "echo off"]
EOF
```

Run: `MANDO_CONFIG=/tmp/mando-face-test.toml cargo run` をバックグラウンド起動し、
`curl -s localhost:8092/api/devices` で kitchen が `"face":"light"`・fan が `"face":null` を確認。
ブラウザ（またはヘッドレス Chromium）で `http://127.0.0.1:8092/` を開き、
「💡 照明」セクションに キッチン照明が 💡 タイル・状態「点灯」で出る／「🔌 スイッチ」
セクションに 換気扇が ⏻ タイル・状態「オフ」で出る／💡 タップで on/off がトグルする、を確認。
ブラウザ目視ができない環境なら curl の `face` 値確認 + 生成 HTML/JS の分岐確認で代替し
report に明記。確認後サーバ停止。

- [ ] **Step 6: コミット**

```bash
git add index.html
git commit -m "feat: face=light の switch を照明セクションに💡タイルで表示"
```

---

## Task 3: config.example.toml に `face` を追記

**Files:**
- Modify: `config.example.toml`

**Interfaces:**
- Consumes: なし（ドキュメントのみ）。

- [ ] **Step 1: switch 例に `face` の説明を追記**

`config.example.toml` の switch 例ブロック（`# ── ECHONET on/off 機器 …` の節）内、
`# kind  = "switch"` の行の直後に次のコメント行を追加:

```toml
# face = "light"  # 任意・switch 専用。指定すると 💡「照明」セクションに並び状態は「点灯/消灯」。
                  # 未指定なら ⏻「スイッチ」セクション。換気扇・床暖房等は未指定のまま。
```

（実際の TOML はコメント（`#`）なので、上記 2 行をコメントとして挿入する。インデントは
周辺のコメント例に合わせる。）

- [ ] **Step 2: 既存テストが緑のままか確認**

Run: `cargo test --bin mando 2>&1 | tail -3`
Expected: 全 PASS（config.example の変更はコメントのみ）。

- [ ] **Step 3: コミット**

```bash
git add config.example.toml
git commit -m "docs: config.example の switch 例に face の説明を追記"
```

---

## 完了条件

- `cargo test --bin mando` / `cargo clippy -- -D warnings` / `cargo build --release` が緑。
- `face = "light"` の switch が「💡 照明」セクションに 💡 タイルで並び、状態が「点灯/消灯」で出る。
- `face` 無しの switch は「🔌 スイッチ」に ⏻ で残る。
- shutter / light に `face` を書くと config エラー。未知 `face` 値も config エラー。
- switch の挙動（on/off・set 後同期確認・ポーリング）は face の有無に関わらず不変。

## Self-Review メモ

- **Spec coverage:** Face 型・Device.face・検証=Task1、API 公開=Task1 Step7、フロント（アイコン/ラベル/セクション）=Task2、config.example=Task3。仕様の全節に対応タスクあり。
- **型整合:** `Face`（config.rs 定義 → main.rs import）、`DeviceInfo.face`、フロントの `c.face`/`dev.face === "light"` を全タスクで統一。
- **退行防止:** 既存の light セクションは `lights.length || switchLights.length` で従来どおり出る。素の switch のセクション条件は `switches` → `plainSwitches` に変わるが face 無し switch は従来通り。renderState は face==="light" 以外の switch を SWITCH_LABEL のまま維持。
