# 色選択ボトムシート + 任意色指定 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** light タイルの色玉行を「色を変える」ボタン 1 個に置換し、ボトムシートで preset 選択 + 虹/こさスライダーによる任意色指定（`POST /api/devices/{name}/color`）を可能にする。

**Architecture:** config に light 専用の任意フィールド `color`（`{color}` プレースホルダを 1 個含むコマンドテンプレ配列）を追加。サーバは hex を厳密検証してから argv 置換のみで exec し、light 例外どおり `{"action": ...}` だけを返す。UI はページ共有のボトムシート 1 枚を開くたびに対象 device へ組み替え、スライダーは change（離した時）にのみ送信する。

**Tech Stack:** Rust (axum 0.7, tokio, serde) + 素の HTML/CSS/JS（`index.html` 焼き込み）。

**Spec:** `docs/superpowers/specs/2026-07-10-color-sheet-arbitrary-color-design.md`

## Global Constraints

- プロトコル・下層コマンドの知識を mando 本体に持ち込まない（バックエンド非依存）。`{color}` 置換は argv のみ、シェルを経由しない。
- hex 検証はサーバ側で `^#[0-9a-fA-F]{6}$` 相当を厳密に。不正は **400** で exec に到達させない。
- 色操作の成功レスポンスは他の light 操作と同じ **`{"action": "<ExecOutcome>"}` のみ**（state 同梱なし）。
- `color` テンプレの無い device・shutter・未知 device への POST color は **404**。
- スライダー送信は change イベント（離した時）に 1 回だけ。input（ドラッグ中）は送らない。
- HSV の V は常に 100% 固定。明度は UI に出さない・送らない。
- 任意色は保存しない（localStorage・config 書き込みなし）。「セッションで最後に押した色」は JS メモリ上のみ。
- コミットメッセージ末尾: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
- 各タスク完了時に `cargo test` 全通過 + `cargo clippy -- -D warnings` クリーン。

---

### Task 1: config — light 任意フィールド `color` と検証

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`

**Interfaces:**
- Produces: `Device.color: Option<Vec<String>>`（serde default）、`Device::color_cmd(&self) -> Option<&[String]>`、`ConfigError::ColorPlaceholder { device: String, count: usize }`
- 検証規則: light の `color` は配列全体で `{color}` プレースホルダをちょうど 1 個含む（0 個・2 個以上は `ColorPlaceholder`、空配列は `EmptyCommand`）。shutter に `color` があれば `ForbiddenField { field: "color" }`。

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `mod tests` 末尾に追加:

```rust
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
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --lib config::tests::color -- --nocapture` 相当として `cargo test color_`
Expected: コンパイルエラー（`color_cmd` / `ColorPlaceholder` 未定義）で FAIL。

- [ ] **Step 3: 最小実装**

`src/config.rs` の `Device` struct、`off` フィールドの後（`presets` の前）に追加:

```rust
    /// 任意色コマンドテンプレ（light のみ・任意）。{color} プレースホルダを
    /// 配列全体でちょうど 1 個含み、検証済み hex（例 "#ff69b4"）に置換して exec される。
    #[serde(default)]
    pub color: Option<Vec<String>>,
```

`impl Device` に accessor 追加（`preset_cmd` の後）:

```rust
    pub fn color_cmd(&self) -> Option<&[String]> {
        self.color.as_deref()
    }
```

`ConfigError` enum にバリアント追加:

```rust
    ColorPlaceholder { device: String, count: usize },
```

`Display` impl に追加:

```rust
            ConfigError::ColorPlaceholder { device, count } => {
                write!(f, "device {device}: color テンプレは {{color}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
```

`validate()` の `Kind::Shutter` アームに追加（`forbid(&d.name, "off", &d.off)?;` の直後）:

```rust
                    forbid(&d.name, "color", &d.color)?;
```

`validate()` の `Kind::Light` アームに追加（preset ループの後）:

```rust
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
```

既存テスト `default_label_is_name` の `Device` リテラルにフィールド追加（`off: None,` の後）:

```rust
            color: None,
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS、clippy クリーン。

- [ ] **Step 5: config.example.toml にサンプル追記**

light 例の `# off   = ["mat", "off", "--node", "5"]` 行の直後に追加:

```toml
# 任意色スライダー用テンプレ（任意）。{color} が検証済み hex（例 "#ff69b4"）に
# 置換されて exec される。{color} は配列全体でちょうど 1 個。指定すると UI の
# 色選択シートに「すきな色」スライダーが出る（shutter には書けない）。
# color = ["mat", "color", "--node", "5", "--rgb", "{color}"]
```

wire group セクション末尾の `# （preset も同様に ...）` 行の直後に追加:

```toml
# color = ["mat", "group", "color", "--group", "10", "--rgb", "{color}"]
```

- [ ] **Step 6: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat: config に light 任意色テンプレ color を追加

{color} プレースホルダをちょうど 1 個含むコマンド配列。shutter では拒否。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: API — `POST /api/devices/{name}/color` と `color_supported`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Device::color_cmd() -> Option<&[String]>`（Task 1）
- Produces: `POST /api/devices/{name}/color`、JSON body `{"color": "#rrggbb"}` → 200 `{"action": "<ExecOutcome>"}` / 400（不正 hex）/ 404（テンプレ無し・shutter・未知 device）。`GET /api/devices` の各要素に `color_supported: bool`。

- [ ] **Step 1: 失敗するテストを書く**

`src/main.rs` の `mod tests` 内 `test_app()` の config を変更。既存 `light` device に `color` テンプレを追加し、テンプレ無し light `plain` を追加する（`[[device]] name = "shutter"` ブロックの前に挿入）:

```rust
            [[device]]
            name = "light"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            color = ["sh", "-c", "test \"$1\" = '#ff69b4' && printf '{}'", "sh", "{color}"]
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            color = "#ffd9a0"
            cmd   = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "plain"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
```

（偽装 sh は `$1` が `#ff69b4` のときだけ exit 0。置換値が argv にそのまま渡ることを終了コードで観測できる。）

JSON body 付きリクエスト用ヘルパを `call` の後に追加:

```rust
    async fn call_json(method: &str, path: &str, body: &str) -> (axum::http::StatusCode, Value) {
        let res = router(test_app())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }
```

テストを `mod tests` 末尾に追加:

```rust
    #[tokio::test]
    async fn color_valid_hex_returns_action_only() {
        let (st, v) = call_json("POST", "/api/devices/light/color", r##"{"color":"#ff69b4"}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // light 例外: state は同梱しない。
        assert!(v.get("state").is_none());
    }

    #[tokio::test]
    async fn color_substitution_reaches_argv() {
        // 偽装 sh は "$1" = "#ff69b4" のときだけ成功する。別の正常 hex を送ると
        // 置換値がそのまま argv に渡っていれば failed になる。
        let (st, v) = call_json("POST", "/api/devices/light/color", r##"{"color":"#00ff00"}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "failed");
    }

    #[tokio::test]
    async fn color_invalid_hex_is_400() {
        for body in [
            r##"{"color":"#GGGGGG"}"##,
            r##"{"color":"red"}"##,
            r##"{"color":"#fff"}"##,
            r##"{"color":"#ff69b4aa"}"##,
        ] {
            let (st, _) = call_json("POST", "/api/devices/light/color", body).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    #[tokio::test]
    async fn color_without_template_is_404() {
        // テンプレ無し light / shutter / 未知 device はすべて既存の kind 不整合と同じ 404。
        for path in [
            "/api/devices/plain/color",
            "/api/devices/shutter/color",
            "/api/devices/ghost/color",
        ] {
            let (st, _) = call_json("POST", path, r##"{"color":"#ff69b4"}"##).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "path: {path}");
        }
    }

    #[tokio::test]
    async fn devices_list_has_color_supported() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let find = |n: &str| arr.iter().find(|d| d["name"] == n).unwrap();
        assert_eq!(find("light")["color_supported"], true);
        assert_eq!(find("plain")["color_supported"], false);
        assert_eq!(find("shutter")["color_supported"], false);
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test color_ devices_list`
Expected: `/color` ルート未定義で 404 になり `color_valid_hex_returns_action_only` FAIL、`color_supported` 欠落で `devices_list_has_color_supported` FAIL。

- [ ] **Step 3: 最小実装**

`src/main.rs` の import を変更: `use serde::Serialize;` → `use serde::{Deserialize, Serialize};`

`router()` にルート追加（`presets/:preset` 行の直後）:

```rust
        .route("/api/devices/:name/color", post(color_device))
```

`DeviceInfo` にフィールド追加（`presets` の前）:

```rust
    /// 任意色（color テンプレ）に対応しているか。UI がスライダーの出し分けに使う。
    color_supported: bool,
```

`list_devices` の struct 生成に追加（`stop: ...` の後）:

```rust
            color_supported: d.color_cmd().is_some(),
```

`preset_device` の後にハンドラ追加:

```rust
#[derive(Deserialize)]
struct ColorReq {
    color: String,
}

/// "#rrggbb"（大文字小文字可）のみ許す。検証済みの値だけが argv 置換に到達する。
fn valid_hex_color(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(|c| c.is_ascii_hexdigit())
}

/// 任意色 exec。テンプレの {color} を検証済み hex に置換して実行し、
/// 送信結果のみ返す（light 例外: state は UI が追いつき取得）。
async fn color_device(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Json(req): Json<ColorReq>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    // color テンプレの無い device（shutter 含む）は既存の kind 不整合と同じ 404。
    let Some(template) = device.color_cmd() else {
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
    if !valid_hex_color(&req.color) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"invalid color\",\"color\":{}}}",
                json_str(&req.color)
            ),
        )
            .into_response();
    }
    let cmd: Vec<String> = template
        .iter()
        .map(|s| s.replace("{color}", &req.color))
        .collect();
    Json(run_light_action(&app, device, &cmd).await).into_response()
}
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS、clippy クリーン。

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: POST /api/devices/{name}/color と color_supported を追加

hex はサーバ側で厳密検証（不正は 400、exec に到達させない）。置換は
argv のみでシェル非経由。返りは light 例外どおり action のみ。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: UI — 色変更ボタン + 色選択ボトムシート

**Files:**
- Modify: `index.html`

**Interfaces:**
- Consumes: `GET /api/devices` の `color_supported: bool` と既存 `presets[]`（Task 2）、`POST /api/devices/{name}/color` body `{"color": "#rrggbb"}` → `{"action": ...}`（Task 2）、既存 `presetAct` / `lightAct` / `setDeviceBusy` / `scheduleLightCatchup` / `ACTION_MSG`。
- Produces: なし（最終タスク）。

- [ ] **Step 1: CSS 差し替え**

`index.html` の `<style>` 内、以下のブロックを**丸ごと削除**（`.swatches` 〜 `button.chip.sel` と `button.chip` 本体。chip は swatches 専用だったため）:

```css
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

```css
  .swatches { display: flex; flex-wrap: wrap; gap: 6px; justify-content: center; margin-top: 7px; }
  .swatches:empty { display: none; }
  button.sw {
    appearance: none; width: 28px; height: 28px; border-radius: 50%;
    border: 2px solid transparent; padding: 0; cursor: pointer; opacity: .8;
    touch-action: manipulation; user-select: none;
    transition: transform var(--tap), opacity var(--tap), border-color var(--tap);
  }
  button.sw:active:not(:disabled) { transform: scale(.9); }
  button.sw:disabled { opacity: .35; cursor: progress; }
  button.sw.sel { border-color: #ffca7a; opacity: 1; }
  button.chip.sel { border-color: #ffca7a; }
```

削除した位置（`.tile.lit .status .label { color: #ffca7a; }` の直後）に以下を追加:

```css
  /* ── 色変更ボタン + 色選択ボトムシート ─────────────── */
  button.colorbtn {
    appearance: none; width: 100%; min-height: 44px; margin-top: 7px;
    border: 1px solid var(--line2); border-radius: 12px;
    background: rgba(255,255,255,.05); color: var(--fg);
    font-size: 13px; font-weight: 700; cursor: pointer;
    display: flex; align-items: center; justify-content: center; gap: 8px;
    touch-action: manipulation; user-select: none;
    transition: transform var(--tap), opacity var(--tap), background var(--tap);
  }
  button.colorbtn:active:not(:disabled) { transform: scale(.97); background: rgba(255,255,255,.10); }
  button.colorbtn:disabled { opacity: .4; cursor: progress; }
  .cdot {
    width: 16px; height: 16px; border-radius: 50%; flex: none;
    border: 1px solid rgba(255,255,255,.25);
    background: conic-gradient(#f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00);
  }
  .dim {
    position: fixed; inset: 0; z-index: 40; background: rgba(0,0,0,.55);
    opacity: 0; pointer-events: none; transition: opacity .2s ease;
  }
  .dim.show { opacity: 1; pointer-events: auto; }
  .sheet {
    position: fixed; left: 0; right: 0; bottom: 0; z-index: 41;
    max-width: 560px; margin: 0 auto;
    background: var(--bg2); border: 1px solid var(--line2); border-bottom: none;
    border-radius: 20px 20px 0 0;
    padding: 16px 18px calc(18px + env(safe-area-inset-bottom));
    box-shadow: 0 -12px 40px rgba(0,0,0,.5);
    transform: translateY(100%); transition: transform .22s cubic-bezier(.2,.8,.3,1);
  }
  .sheet.show { transform: none; }
  .sheet h2 { margin: 0 0 8px; font-size: 15px; font-weight: 800; }
  button.crow {
    appearance: none; display: flex; align-items: center; gap: 12px;
    width: 100%; min-height: 48px; padding: 0 8px;
    background: none; border: none; border-radius: 12px;
    color: var(--fg); font-size: 14px; font-weight: 600; cursor: pointer;
    touch-action: manipulation; user-select: none;
  }
  button.crow:active:not(:disabled) { background: rgba(255,255,255,.07); }
  button.crow:disabled { opacity: .4; cursor: progress; }
  .crow .pdot {
    width: 24px; height: 24px; border-radius: 50%; flex: none;
    border: 2px solid transparent;
  }
  .crow.sel .pdot { border-color: #ffca7a; box-shadow: 0 0 0 2px rgba(255,202,122,.3); }
  .crow .pdot.nocolor { border: 1px dashed var(--line2); }
  .sheet .divider {
    display: flex; align-items: center; gap: 10px;
    font-size: 12px; color: var(--muted); font-weight: 700;
    letter-spacing: .06em; margin: 12px 0 4px;
  }
  .sheet .divider::after { content: ""; flex: 1; height: 1px; background: var(--line); }
  .sliders { display: flex; flex-direction: column; gap: 4px; padding: 2px 2px 0; }
  input.cslider {
    appearance: none; -webkit-appearance: none;
    width: 100%; height: 44px; margin: 0; background: none; touch-action: pan-y;
  }
  input.cslider:disabled { opacity: .4; cursor: progress; }
  input.cslider::-webkit-slider-runnable-track {
    height: 14px; border-radius: 999px; border: 1px solid var(--line2);
    background: var(--track, #888);
  }
  input.cslider::-webkit-slider-thumb {
    -webkit-appearance: none; appearance: none;
    width: 44px; height: 44px; margin-top: -16px; border-radius: 50%;
    border: 3px solid #fff; background: var(--thumb, #888);
    box-shadow: 0 2px 8px rgba(0,0,0,.5);
  }
  input.cslider::-moz-range-track {
    height: 14px; border-radius: 999px; border: 1px solid var(--line2);
    background: var(--track, #888);
  }
  input.cslider::-moz-range-thumb {
    width: 38px; height: 38px; border-radius: 50%;
    border: 3px solid #fff; background: var(--thumb, #888);
    box-shadow: 0 2px 8px rgba(0,0,0,.5);
  }
  input.cslider.hue {
    --track: linear-gradient(90deg, #f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00);
  }
```

- [ ] **Step 2: `api()` に JSON body 対応を追加**

既存:

```js
async function api(method, path) {
  const res = await fetch(path, { method, headers: { "accept": "application/json" } });
  if (!res.ok) throw new Error("HTTP " + res.status);
  return res.json();
}
```

を以下に差し替え:

```js
async function api(method, path, body) {
  const opts = { method, headers: { "accept": "application/json" } };
  if (body !== undefined) {
    opts.headers["content-type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  if (!res.ok) throw new Error("HTTP " + res.status);
  return res.json();
}
```

`lightAct` のシグネチャと POST 行を変更:

```js
async function lightAct(name, verb, path, body) {
```

```js
    const view = await api("POST", path, body);
```

（他の呼び出し元は body 未指定のままで挙動不変。）

- [ ] **Step 3: `buildLightTile` の色玉行を colorbtn に置換**

既存の `buildLightTile` 関数全体を以下に差し替え:

```js
/* ── light タイル（電球ボタン = スイッチ + 色変更ボタン）──── */
function buildLightTile(dev) {
  const el = document.createElement("div");
  el.className = "tile";
  el.innerHTML = `
    <button class="bulb" type="button" aria-label="点灯/消灯">💡</button>
    <div class="tname"></div>
    <div class="status unknown"><span class="label">不明</span><span class="msg"></span></div>
  `;
  el.querySelector(".tname").textContent = dev.label;
  // preset も color テンプレも無い light はボタン自体を出さない。
  if (dev.presets.length || dev.color_supported) {
    const btn = document.createElement("button");
    btn.className = "colorbtn";
    btn.type = "button";
    btn.innerHTML = `<span class="cdot"></span>色を変える`;
    btn.addEventListener("click", () => openSheet(dev));
    el.appendChild(btn);
  }
  const c = {
    kind: "light",
    rootEl: el,
    statusEl: el.querySelector(".status"),
    labelEl: el.querySelector(".status .label"),
    msgEl: el.querySelector(".status .msg"),
    dotEl: el.querySelector(".cdot"), // colorbtn 内の「最後に押した色」ドット（無ければ null）
    buttons: [...el.querySelectorAll("button")], // bulb + colorbtn を busy 対象に
    state: "unknown",
    lastSel: null, // このセッションで最後に押した色（"preset:<name>" | "custom"）
    lastHue: 30,   // シート再訪時のスライダー初期位置
    lastSat: 100,
    catchupTimer: null, // 追いつき取得タイマー（device ごとに 1 本、連打時は張り直し）
  };
  el.querySelector(".bulb").addEventListener("click", () =>
    deviceAct(dev.name, c.state === "on" ? "off" : "on")
  );
  cards.set(dev.name, c);
  return el;
}
```

- [ ] **Step 4: ボトムシート + colorAct を追加**

`presetAct` 関数の直前に以下を追加:

```js
/* ── 色選択ボトムシート（ページ共有の 1 枚、開くたびに対象 device へ組み替え）── */
const dimEl = document.createElement("div");
dimEl.className = "dim";
const sheetEl = document.createElement("div");
sheetEl.className = "sheet";
document.body.append(dimEl, sheetEl);
let sheetDevice = null; // 開いている対象 device 名（busy 制御用）
dimEl.addEventListener("click", closeSheet);

function closeSheet() {
  dimEl.classList.remove("show");
  sheetEl.classList.remove("show");
  sheetDevice = null;
}

/* HSV → "#rrggbb"。V は常に 100%（mat group color は明度を捨てる仕様。UI にも出さない）。 */
function hsvToHex(h, s) {
  const f = (n) => {
    const k = (n + h / 60) % 6;
    return Math.round((1 - s * Math.max(0, Math.min(k, 4 - k, 1))) * 255)
      .toString(16).padStart(2, "0");
  };
  return "#" + f(5) + f(3) + f(1);
}

/* 「セッションで最後に押した色」を記録し、タイルの colorbtn ドットへ反映。 */
function markLastColor(c, sel, cssColor) {
  c.lastSel = sel;
  if (cssColor && c.dotEl) c.dotEl.style.background = cssColor;
}

function openSheet(dev) {
  const c = cards.get(dev.name);
  if (!c) return;
  sheetDevice = dev.name;
  sheetEl.innerHTML = "";

  const h = document.createElement("h2");
  h.textContent = `${dev.label} の色`;
  sheetEl.appendChild(h);

  for (const p of dev.presets) {
    const row = document.createElement("button");
    row.className = "crow";
    row.type = "button";
    const dot = document.createElement("span");
    dot.className = "pdot" + (p.color ? "" : " nocolor");
    if (p.color) dot.style.background = p.color;
    const nm = document.createElement("span");
    nm.textContent = p.label;
    row.append(dot, nm);
    // .sel リングは「セッションで最後に押した色」。現在色の主張はしない（原則 7）。
    if (c.lastSel === "preset:" + p.name) row.classList.add("sel");
    row.addEventListener("click", () => {
      for (const r of sheetEl.querySelectorAll(".crow.sel")) r.classList.remove("sel");
      row.classList.add("sel");
      markLastColor(c, "preset:" + p.name, p.color);
      presetAct(dev.name, p.name);
    });
    sheetEl.appendChild(row);
  }

  if (dev.color_supported) {
    const div = document.createElement("div");
    div.className = "divider";
    div.textContent = "すきな色";
    sheetEl.appendChild(div);

    const wrap = document.createElement("div");
    wrap.className = "sliders";
    const hue = document.createElement("input");
    hue.type = "range"; hue.min = "0"; hue.max = "360"; hue.step = "1";
    hue.value = String(c.lastHue);
    hue.className = "cslider hue";
    hue.setAttribute("aria-label", "いろあい");
    const sat = document.createElement("input");
    sat.type = "range"; sat.min = "0"; sat.max = "100"; sat.step = "1";
    sat.value = String(c.lastSat);
    sat.className = "cslider";
    sat.setAttribute("aria-label", "こさ");
    wrap.append(hue, sat);
    sheetEl.appendChild(wrap);

    // こさスライダーの右端色（と各つまみ）は現在の色相に追従させる。
    const paint = () => {
      const hv = +hue.value, sv = +sat.value / 100;
      hue.style.setProperty("--thumb", hsvToHex(hv, 1));
      sat.style.setProperty("--track", `linear-gradient(90deg, #fff, ${hsvToHex(hv, 1)})`);
      sat.style.setProperty("--thumb", hsvToHex(hv, sv));
    };
    // ドラッグ中（input）は描画のみ。送信は離した時（change）に 1 回だけ
    // — exec 直列（Semaphore(1)）と相性を保つ。
    const send = () => {
      c.lastHue = +hue.value;
      c.lastSat = +sat.value;
      const hex = hsvToHex(+hue.value, +sat.value / 100);
      for (const r of sheetEl.querySelectorAll(".crow.sel")) r.classList.remove("sel");
      markLastColor(c, "custom", hex);
      colorAct(dev.name, hex);
    };
    hue.addEventListener("input", paint);
    sat.addEventListener("input", paint);
    hue.addEventListener("change", send);
    sat.addEventListener("change", send);
    paint();
  }

  dimEl.classList.add("show");
  sheetEl.classList.add("show");
}

/* ── 任意色実行（light 専用）─────────────────────── */
async function colorAct(name, hex) {
  return lightAct(
    name,
    VERB.preset,
    `/api/devices/${encodeURIComponent(name)}/color`,
    { color: hex }
  );
}
```

- [ ] **Step 5: busy 制御にシートを組み込む**

既存の `setDeviceBusy` / `setAllButtonsDisabled` を以下に差し替え:

```js
/* シートが対象 device（name=null なら無条件）を開いていれば、シート内の操作も止める。 */
function setSheetDisabled(name, busy) {
  if (sheetDevice !== null && (name === null || sheetDevice === name)) {
    for (const el of sheetEl.querySelectorAll("button, input")) el.disabled = busy;
  }
}

function setDeviceBusy(name, busy) {
  const c = cards.get(name);
  if (c) for (const b of c.buttons) b.disabled = busy;
  setSheetDisabled(name, busy);
}

function setAllButtonsDisabled(disabled) {
  for (const c of cards.values()) for (const b of c.buttons) b.disabled = disabled;
  for (const g of groups) for (const b of g.buttons) b.disabled = disabled;
  setSheetDisabled(null, disabled);
}
```

- [ ] **Step 6: ビルドと既存テスト・clippy の確認**

Run: `cargo build && cargo test && cargo clippy -- -D warnings`
Expected: ビルド成功（`include_str!` で index.html が焼き込まれる）、既存テスト全 PASS、clippy クリーン。

- [ ] **Step 7: 偽装 config で E2E スモーク**

```bash
cat > /tmp/claude-1000/-home-noguk-ghq-github-com-nogu3-mando/339d5d90-0a35-4d9c-8fc5-df5d83dbe6f8/scratchpad/smoke.toml <<'EOF'
bind = "127.0.0.1:18901"
[[device]]
name = "light"
kind = "light"
get_state = ["sh", "-c", "printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]
color = ["sh", "-c", "printf '{}' # $1", "sh", "{color}"]
[[device.preset]]
name  = "warm"
label = "電球色"
color = "#ffd9a0"
cmd   = ["sh", "-c", "printf '{}'"]
EOF
MANDO_CONFIG=/tmp/claude-1000/-home-noguk-ghq-github-com-nogu3-mando/339d5d90-0a35-4d9c-8fc5-df5d83dbe6f8/scratchpad/smoke.toml ./target/debug/mando &
sleep 1
curl -s http://127.0.0.1:18901/api/devices        # color_supported: true を確認
curl -s -X POST -H 'content-type: application/json' -d '{"color":"#ff69b4"}' \
  http://127.0.0.1:18901/api/devices/light/color   # {"action":"success"} を確認
curl -s -o /dev/null -w '%{http_code}\n' -X POST -H 'content-type: application/json' \
  -d '{"color":"red"}' http://127.0.0.1:18901/api/devices/light/color  # 400 を確認
kill %1
```

Expected: `color_supported: true`、`{"action":"success"}`、`400`。

- [ ] **Step 8: Commit**

```bash
git add index.html
git commit -m "feat: 色玉行を色変更ボタン化し色選択ボトムシートを追加

preset 行 + 虹/こさスライダー（HSV、V=100% 固定）。送信は離した時のみ。
任意色は保存しない（セッション内の最後に押した色をドットに出すだけ）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 実機確認（デプロイ後・手動）

コード外の残作業。実装タスクには含めない:

- jarvis の実 config の light に `color = ["<mat 絶対パス>", "group", "color", "--group", "living_lights", "--rgb", "{color}"]` を追記して mando を再起動。
- スマホで: シート開閉、preset 行 48px、スライダー離しで色が変わる（~1 秒）、連続ドラッグでも送信は離した回数だけ、busy 中はシート内ボタンも無効。
