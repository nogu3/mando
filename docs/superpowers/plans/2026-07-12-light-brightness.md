# light 明るさ（調光）制御 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** light に明るさ（1〜100%）スライダーを追加し、`color` と対称の config テンプレ + API + 既存の色シート内スライダーで調光できるようにする。

**Architecture:** `color` 機構の完全な鏡像。config に `brightness` コマンドテンプレ（`{brightness}` プレースホルダ 1 個）を足し、`POST /api/devices/{name}/brightness` が 1〜100 の整数を検証して argv 置換し exec、送信結果のみ返す（light 例外）。`GET /api/devices` に `brightness_supported` を足し、UI は既存の色ボトムシートに明るさスライダーブロックを追加する。mando 本体は下層コマンドを知らない（バックエンド非依存）。

**Tech Stack:** Rust（axum / serde / toml）、バニラ JS の単一 `index.html`（`include_str!` 焼き込み）。

## Global Constraints

- プロトコルを直接喋らない。exec するのは config のコマンド配列のみ（バックエンド非依存）。
- subprocess は既存の `Executor`（内部で Semaphore(1) 直列化）を必ず経由する。
- light 操作 POST は送信結果（`{"action":"<ExecOutcome>"}`）のみ返す。state は同梱しない（設計原則 7 の light 例外）。
- argv 置換のみ（シェル非経由）。検証済みの値以外を下層に渡さない。
- 明るさは 1〜100 の整数（%）。0・101 以上・非整数は 400。
- `index.html` はバイナリ焼き込み。UI 変更は手動実機確認（ユニットテストは無い）。
- コミットはこのセッションで編集したファイルのみを `git add` する。

---

### Task 1: config に `brightness` テンプレ + 検証を追加

`color` と対称の任意フィールド `brightness` を Device に足し、light では `{brightness}` を
ちょうど 1 個含むことを検証、shutter では禁止する。

**Files:**
- Modify: `src/config.rs`（Device 定義・アクセサ・ConfigError・Display・validate・テスト）
- Modify: `config.example.toml`（brightness テンプレのコメント例を追記）

**Interfaces:**
- Produces:
  - `Device.brightness: Option<Vec<String>>`（serde `#[serde(default)]`）
  - `Device::brightness_cmd(&self) -> Option<&[String]>`
  - `ConfigError::BrightnessPlaceholder { device: String, count: usize }`

- [ ] **Step 1: brightness 検証の失敗テストを書く**

`src/config.rs` の `#[cfg(test)] mod tests` 内、既存の `color_empty_rejected`
テストの後に追記する（`color_*` テスト群を鏡写しにする）:

```rust
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
```

- [ ] **Step 2: テストがコンパイルエラー/失敗することを確認**

Run: `cargo test --lib config 2>&1 | tail -20`
Expected: コンパイルエラー（`brightness` フィールド・`brightness_cmd`・`BrightnessPlaceholder` が未定義）。

- [ ] **Step 3: Device に `brightness` フィールドを追加**

`src/config.rs` の `Device` 構造体、`color` フィールド（`pub color: Option<Vec<String>>,`）の
直後・`presets` の前に追加:

```rust
    /// 明るさ（調光）コマンドテンプレ（light のみ・任意）。{brightness} プレースホルダを
    /// 配列全体でちょうど 1 個含み、検証済みの整数 1〜100 に置換して exec される。
    #[serde(default)]
    pub brightness: Option<Vec<String>>,
```

- [ ] **Step 4: `brightness_cmd()` アクセサを追加**

`impl Device` 内、`color_cmd` の直後に追加:

```rust
    pub fn brightness_cmd(&self) -> Option<&[String]> {
        self.brightness.as_deref()
    }
```

- [ ] **Step 5: ConfigError バリアントと Display を追加**

`enum ConfigError` に `ColorPlaceholder` の直後で追加:

```rust
    BrightnessPlaceholder { device: String, count: usize },
```

`impl std::fmt::Display for ConfigError` の match、`ColorPlaceholder` アームの直後に追加:

```rust
            ConfigError::BrightnessPlaceholder { device, count } => {
                write!(f, "device {device}: brightness テンプレは {{brightness}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
```

- [ ] **Step 6: shutter で brightness を禁止**

`validate()` の `Kind::Shutter` アーム、`forbid(&d.name, "color", &d.color)?;` の直後に追加:

```rust
                    forbid(&d.name, "brightness", &d.brightness)?;
```

- [ ] **Step 7: light で brightness プレースホルダを検証**

`validate()` の `Kind::Light` アーム、既存の `if let Some(color) = &d.color { ... }`
ブロックの直後に追加:

```rust
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
```

- [ ] **Step 8: 既存の Device 構造体リテラルに `brightness: None` を追加**

`default_label_is_name` テスト内の `Device { ... }` リテラル、`color: None,` の直後に追加
（フィールド追加でこのリテラルがコンパイルエラーになるため必須）:

```rust
            brightness: None,
```

- [ ] **Step 9: テストが通ることを確認**

Run: `cargo test --lib config 2>&1 | tail -20`
Expected: PASS（`brightness_*` 5 件を含む config テスト全通過）。

- [ ] **Step 10: config.example.toml に brightness コメント例を追記**

`config.example.toml` の任意色スライダー用テンプレのコメントブロック
（`# color = ["mat", "color", "--node", "5", "--rgb", "{color}"]` の行）の直後に追加:

```toml
# 明るさ（調光）スライダー用テンプレ（任意）。{brightness} が検証済みの整数 1〜100 に
# 置換されて exec される。{brightness} は配列全体でちょうど 1 個。指定すると UI の
# あかり調整シートに明るさスライダーが出る（shutter には書けない）。% → 機器生値の
# 変換が要る場合は mat 側で吸収する（mando は % のまま渡す）。
# brightness = ["mat", "level", "--node", "5", "--percent", "{brightness}"]
```

- [ ] **Step 11: コミット**

```bash
git add src/config.rs config.example.toml
git commit -m "$(cat <<'EOF'
feat: config に light の brightness テンプレ + 検証を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `POST /api/devices/{name}/brightness` と `brightness_supported` を追加

1〜100 の整数を厳密検証して `{brightness}` を置換 exec、送信結果のみ返す。
`GET /api/devices` に `brightness_supported` を足す。`color_device` の鏡像。

**Files:**
- Modify: `src/main.rs`（ルート登録・DeviceInfo・list_devices・BrightnessReq・brightness_device・テスト）

**Interfaces:**
- Consumes: `Device::brightness_cmd() -> Option<&[String]>`（Task 1）
- Produces:
  - ルート `POST /api/devices/:name/brightness` → `brightness_device`
  - `DeviceInfo.brightness_supported: bool`

- [ ] **Step 1: API の失敗テストを書く**

`src/main.rs` の `#[cfg(test)] mod tests` 内、既存の `devices_list_has_color_supported`
テストの後に追記する（`color_*` テスト群を鏡写しにする）。偽装 sh は `"$1" = "50"` の
ときだけ成功する（テスト用 config は Step 4 で足す）:

```rust
    #[tokio::test]
    async fn brightness_valid_returns_action_only() {
        let (st, v) = call_json("POST", "/api/devices/light/brightness", r##"{"brightness":50}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // light 例外: state は同梱しない。
        assert!(v.get("state").is_none());
    }

    #[tokio::test]
    async fn brightness_substitution_reaches_argv() {
        // 偽装 sh は "$1" = "50" のときだけ成功する。別の正常値を送ると
        // 置換値がそのまま argv に渡っていれば failed になる。
        let (st, v) = call_json("POST", "/api/devices/light/brightness", r##"{"brightness":75}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "failed");
    }

    #[tokio::test]
    async fn brightness_invalid_is_400() {
        for body in [
            r##"{"brightness":0}"##,
            r##"{"brightness":101}"##,
            r##"{"brightness":"50"}"##,
            r##"{"brightness":50.5}"##,
            r##"{"brightness":-1}"##,
        ] {
            let (st, _) = call_json("POST", "/api/devices/light/brightness", body).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    #[tokio::test]
    async fn brightness_without_template_is_404() {
        // テンプレ無し light / shutter / 未知 device はすべて既存の kind 不整合と同じ 404。
        for path in [
            "/api/devices/plain/brightness",
            "/api/devices/shutter/brightness",
            "/api/devices/ghost/brightness",
        ] {
            let (st, _) = call_json("POST", path, r##"{"brightness":50}"##).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "path: {path}");
        }
    }

    #[tokio::test]
    async fn devices_list_has_brightness_supported() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let find = |n: &str| arr.iter().find(|d| d["name"] == n).unwrap();
        assert_eq!(find("light")["brightness_supported"], true);
        assert_eq!(find("plain")["brightness_supported"], false);
        assert_eq!(find("shutter")["brightness_supported"], false);
    }
```

- [ ] **Step 2: テストがコンパイルエラー/失敗することを確認**

Run: `cargo test --lib -- brightness 2>&1 | tail -20`
Expected: コンパイルエラー（ルート・`brightness_supported`・テスト config に brightness が無い）。

- [ ] **Step 3: ルートを登録**

`src/main.rs` の `Router::new()` チェーン、`.route("/api/devices/:name/color", post(color_device))`
の直後に追加:

```rust
        .route("/api/devices/:name/brightness", post(brightness_device))
```

- [ ] **Step 4: テスト用 config の light に brightness テンプレを追加**

`test_app()` の TOML、`light` device の `color = [...]` 行の直後
（`[[device.preset]]` の前）に追加:

```
            brightness = ["sh", "-c", "test \"$1\" = '50' && printf '{}'", "sh", "{brightness}"]
```

- [ ] **Step 5: DeviceInfo に `brightness_supported` を追加**

`struct DeviceInfo` の `color_supported: bool,` の直後に追加:

```rust
    /// 明るさ（brightness テンプレ）に対応しているか。UI がスライダーの出し分けに使う。
    brightness_supported: bool,
```

`list_devices` の map クロージャ、`color_supported: d.color_cmd().is_some(),` の直後に追加:

```rust
            brightness_supported: d.brightness_cmd().is_some(),
```

- [ ] **Step 6: BrightnessReq と brightness_device ハンドラを追加**

`src/main.rs` の `color_device` 関数の直後に追加。body は `serde_json::Value` で受け、
`as_u64()` + 範囲チェックで文字列・小数・範囲外をすべて 400 にする
（axum の型不一致 422 を避け、spec どおり 400 に揃える）:

```rust
#[derive(Deserialize)]
struct BrightnessReq {
    brightness: Value,
}

/// 明るさ exec。テンプレの {brightness} を検証済みの整数 1〜100 に置換して実行し、
/// 送信結果のみ返す（light 例外: state は UI が追いつき取得）。color_device の鏡像。
async fn brightness_device(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Json(req): Json<BrightnessReq>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    // brightness テンプレの無い device（shutter 含む）は既存の kind 不整合と同じ 404。
    let Some(template) = device.brightness_cmd() else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unsupported operation\",\"name\":{}}}",
                json_str(&name)
            ),
        )
            .into_response();
    };
    // JSON 数値の整数のみ受ける。文字列・小数・0・101 以上・負値はすべて 400。
    let Some(level) = req.brightness.as_u64().filter(|n| (1..=100).contains(n)) else {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"invalid brightness\",\"brightness\":{}}}",
                req.brightness
            ),
        )
            .into_response();
    };
    let level = level.to_string();
    let cmd: Vec<String> = template
        .iter()
        .map(|s| s.replace("{brightness}", &level))
        .collect();
    Json(run_light_action(&app, device, &cmd).await).into_response()
}
```

- [ ] **Step 7: テストが通ることを確認**

Run: `cargo test --lib -- brightness 2>&1 | tail -20`
Expected: PASS（`brightness_valid_returns_action_only` / `brightness_substitution_reaches_argv` / `brightness_invalid_is_400` / `brightness_without_template_is_404` / `devices_list_has_brightness_supported`）。

- [ ] **Step 8: 全テスト + clippy**

Run: `cargo test 2>&1 | tail -15 && cargo clippy -- -D warnings 2>&1 | tail -15`
Expected: 全テスト PASS、clippy 警告なし。

- [ ] **Step 9: コミット**

```bash
git add src/main.rs
git commit -m "$(cat <<'EOF'
feat: POST /api/devices/{name}/brightness と brightness_supported を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: 色シートに明るさスライダーを追加（UI）

既存の色ボトムシートを「あかり調整」シートに拡張。シート先頭に明るさスライダーブロックを
足し、`brightness_supported` で出し分け。送信は change イベントで 1 回だけ。

**Files:**
- Modify: `index.html`（colorbtn 文言・openSheet 明るさブロック・brightnessAct・card 状態 lastBright）

**Interfaces:**
- Consumes: `dev.brightness_supported: bool`（Task 2）、既存 `lightAct` / `VERB.preset` / `LIGHT_CATCHUP_MS`

- [ ] **Step 1: colorbtn の表示条件と文言を一般化**

`index.html` の `buildLightCard`（`if (dev.presets.length || dev.color_supported) {` の箇所）を、
明るさ対応も条件に含め、文言を「あかり調整」に変更:

```javascript
  // preset も color も brightness も無い light はボタン自体を出さない。
  if (dev.presets.length || dev.color_supported || dev.brightness_supported) {
    const btn = document.createElement("button");
    btn.className = "colorbtn";
    btn.type = "button";
    btn.innerHTML = `<span class="cdot"></span>あかり調整`;
    btn.addEventListener("click", () => openSheet(dev));
    el.appendChild(btn);
  }
```

- [ ] **Step 2: card 状態に lastBright を追加**

同じ `buildLightCard` の `const c = { ... }` オブジェクト、`lastSat: 100,` の直後に追加:

```javascript
    lastBright: 100, // シート再訪時の明るさスライダー初期位置（現在値は state に出ない）
```

- [ ] **Step 3: シートヘッダ文言を変更**

`openSheet` 内のヘッダ生成 `h.textContent = ` の行を変更:

```javascript
  h.textContent = `${dev.label} のあかり`;
```

- [ ] **Step 4: 明るさスライダーブロックを追加**

`openSheet` 内、ヘッダ `sheetEl.appendChild(h);` の直後・preset ループ（`for (const p of dev.presets)`）
の前に追加。明るさは preset/色より上に置く:

```javascript
  if (dev.brightness_supported) {
    const bwrap = document.createElement("div");
    bwrap.className = "sliders";
    const blabel = document.createElement("div");
    blabel.className = "divider";
    blabel.textContent = "あかりの強さ";
    const bright = document.createElement("input");
    bright.type = "range"; bright.min = "1"; bright.max = "100"; bright.step = "1";
    bright.value = String(c.lastBright);
    bright.className = "cslider";
    bright.setAttribute("aria-label", "あかりの強さ");
    bwrap.appendChild(bright);
    sheetEl.appendChild(blabel);
    sheetEl.appendChild(bwrap);
    // ドラッグ中（input）は送らず、離した時（change）に 1 回だけ送信
    // — exec 直列（Semaphore(1)）と相性を保つ。
    bright.addEventListener("change", () => {
      c.lastBright = +bright.value;
      brightnessAct(dev.name, +bright.value);
    });
  }
```

- [ ] **Step 5: brightnessAct を追加**

`colorAct` 関数の直後に追加:

```javascript
/* ── 明るさ実行（light 専用）─────────────────────── */
async function brightnessAct(name, level) {
  return lightAct(
    name,
    VERB.preset,
    `/api/devices/${encodeURIComponent(name)}/brightness`,
    { brightness: level }
  );
}
```

- [ ] **Step 6: ビルドが通ることを確認（焼き込み構文チェック）**

Run: `cargo build 2>&1 | tail -10`
Expected: PASS（`include_str!` の `index.html` を焼き込んでコンパイル成功）。

- [ ] **Step 7: 実機 UI 確認**

ローカルで `config.toml` の light に `brightness` テンプレを入れて起動し、スマホ/ブラウザで確認:

Run: `RUST_LOG=debug cargo run`
確認項目:
- 「💡 あかり調整」ボタン（文言）でシートが開く
- シート先頭に「あかりの強さ」スライダーが出る（brightness 対応 light のみ）
- スライダーを離すと明るさが変わる（~1 秒）。ドラッグ中は送らない
- busy 中はシート内スライダーも無効（既存 setDeviceBusy 経由）
- brightness 非対応の light ではスライダーブロックが出ない
- 既存の on/off・preset・色スライダー・shutter が無変更で動く

- [ ] **Step 8: コミット**

```bash
git add index.html
git commit -m "$(cat <<'EOF'
feat: 色シートに明るさ（調光）スライダーを追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

**Spec coverage:**
- 値 1〜100% → Task 2 Step 6（`(1..=100)`）、Task 3 Step 4（slider min/max）✓
- config `brightness` テンプレ + `{brightness}` 1 個検証 + shutter 拒否 + 空拒否 → Task 1 ✓
- `POST /brightness`、1〜100 整数検証、範囲外/非整数 400、テンプレ無し/shutter/未知 404、`{"action":...}` のみ → Task 2 ✓
- `brightness_supported` を devices 一覧に → Task 2 Step 5 ✓
- 色シートに明るさブロック、change で 1 回送信、lastBright セッション記憶、ボタン文言「💡 あかり調整」 → Task 3 ✓
- state 読み戻し無し・保存無し・shutter 無変更 → 実装しない（該当タスク無し = 正しい）✓

**Placeholder scan:** なし（全ステップに実コード）。

**Type consistency:** `Device.brightness` / `brightness_cmd()` / `ConfigError::BrightnessPlaceholder` / `DeviceInfo.brightness_supported` / `BrightnessReq.brightness` / route `brightness_device` / JS `brightnessAct` / `lastBright` — Task 間で名称一致を確認済み。`💡` 絵文字はボタン innerHTML には含めず（既存 colorbtn は `.cdot` ドット + テキスト構成）、実機確認項目の文言表現として記載。ボタン文言は「あかり調整」で統一。
