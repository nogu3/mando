# light 操作と状態取得の分離（非同期確認）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** light の操作 POST を「送信結果のみ返す」に変え、state は UI が押下 ~2 秒後に 1 回だけ非同期で追いつき取得する。ボタンが ~2.3 秒塞がる問題を解消する。

**Architecture:** サーバ側は `device.kind == Kind::Light` のとき `run_action`（exec + state 再取得）でなく exec のみの `run_light_action` を呼び、`{"action": "<ExecOutcome>"}` だけ返す。UI 側は light の POST 応答で即ボタン解放し、状態ラベルを「反映中…」にして `setTimeout` ~2000ms 後に `GET state` → `renderState`（device ごとにタイマー 1 本、連打時は張り直し、busy 中はスキップ）。shutter は一切変更しない（設計原則 7 維持）。

**Tech Stack:** Rust (axum, serde) / vanilla JS (index.html 焼き込み) / go-task + cross (deploy)

**Spec:** `docs/superpowers/specs/2026-07-10-light-async-state-design.md`（承認済み）

## Global Constraints

- shutter の経路（POST 応答形・ポーリング・グループ操作）は無変更。壊したら原則 7 違反。
- 404 系（kind 不整合・未知 preset・未知 device）のレスポンスは無変更。
- light の追いつき取得は **1 回だけ**。リトライ・ポーリング復活はしない（spec「やらないこと」）。
- API バージョニング不要（利用者は同梱 UI のみ）。
- コミットは日本語 Conventional Commits（既存ログの体裁に合わせる: `feat:` / `fix:` / `docs:`）。
- 検証コマンド: `cargo test` 全通過 + `cargo clippy -- -D warnings` クリーン。

---

### Task 1: API — light の POST は exec 結果のみ返す

**Files:**
- Modify: `src/main.rs`（`run_action` 定義の直後 ~L235 に追加、`device_op` L300-317、`preset_device` L278-298、tests L515-530）

**Interfaces:**
- Consumes: 既存の `Executor::run`, `ExecOutcome`, `Device`, `Kind`（変更なし）
- Produces: `LightActionView { action: ExecOutcome }`（Serialize、JSON 形 `{"action":"success"}`）と `async fn run_light_action(app: &App, device: &Device, cmd: &[String]) -> LightActionView`。Task 2 の UI はこの `{"action": ...}` 形（`state` キーなし）に依存する。

- [ ] **Step 1: 既存テスト 2 本を書き換え + shutter 回帰テスト 1 本を追加（失敗するテストを先に書く）**

`src/main.rs` の tests モジュールで、`light_on_returns_confirmed_state`（L515-522）と `preset_runs_and_confirms_state`（L524-530）を以下に置き換え、`shutter_open_still_returns_confirmed_state` を新規追加する:

```rust
    #[tokio::test]
    async fn light_on_returns_action_only() {
        let (st, v) = call("POST", "/api/devices/light/on").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // 非同期確認: state は同梱しない（UI が後で 1 回だけ GET state する）。
        assert!(v.get("state").is_none());
        assert!(v.get("exec").is_none());
        assert!(v.get("raw").is_none());
    }

    #[tokio::test]
    async fn preset_returns_action_only() {
        let (st, v) = call("POST", "/api/devices/light/presets/warm").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        assert!(v.get("state").is_none());
    }

    #[tokio::test]
    async fn shutter_open_still_returns_confirmed_state() {
        let (st, v) = call("POST", "/api/devices/shutter/open").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // 設計原則 7: shutter は set 後の同期確認を維持。
        assert_eq!(v["state"], "open");
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test light_on_returns_action_only preset_returns_action_only`
Expected: 2 本とも FAIL（現行は `state` キーが同梱されるため `v.get("state").is_none()` が落ちる）。`shutter_open_still_returns_confirmed_state` は現行実装でも PASS するはず（回帰ガード）。

- [ ] **Step 3: 実装**

`src/main.rs` の `run_action`（L218-234）の直後に追加:

```rust
/// light の set 結果。state は同梱しない — UI が押下 ~2 秒後に 1 回だけ
/// 追いつき取得する（設計原則 7 の light 例外。shutter は run_action を維持）。
#[derive(Serialize)]
struct LightActionView {
    action: ExecOutcome,
}

/// exec のみ実行して送信結果を返す（state 再取得なし）。light 用。
async fn run_light_action(app: &App, device: &Device, cmd: &[String]) -> LightActionView {
    let result = app.executor.run(cmd).await;
    if result.outcome != ExecOutcome::Success {
        tracing::warn!(
            device = %device.name,
            outcome = ?result.outcome,
            stderr = %result.stderr.trim(),
            "set 非成功"
        );
    }
    LightActionView {
        action: result.outcome,
    }
}
```

`device_op`（L300-317）の match を kind 分岐に変える:

```rust
async fn device_op(app: &App, name: &str, op: Op) -> Response {
    let Some(device) = app.config.find(name) else {
        return not_found(name);
    };
    match device_cmd(device, op) {
        // light は exec 結果のみ返す（state は UI が非同期に追いつき取得）。
        Some(cmd) if device.kind == Kind::Light => {
            Json(run_light_action(app, device, &cmd).await).into_response()
        }
        Some(cmd) => Json(run_action(app, device, &cmd).await).into_response(),
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
    }
}
```

`preset_device`（L278-298）の Some 腕を差し替える。preset は config 検証で light 専用（shutter に preset があると ForbiddenField）なので、常に light 経路でよい:

```rust
        // preset は light 専用（config 検証済み）なので exec 結果のみ返す。
        Some(cmd) => Json(run_light_action(&app, device, cmd).await).into_response(),
```

`preset_device` の doc コメント `/// 名前付きプリセット exec → state 再取得（設計原則 7）。` は `/// 名前付きプリセット exec → 送信結果のみ返す（light 例外: state は UI が追いつき取得）。` に更新する。

- [ ] **Step 4: テスト全通過 + clippy を確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全テスト PASS（shutter 系・GET state 系・404 系が不変のまま通ること）、clippy 警告ゼロ。

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: light の POST は送信結果のみ返す（同期 state 確認を分離）"
```

---

### Task 2: UI — light 押下後の非同期追いつき取得

**Files:**
- Modify: `index.html`（`presetAct` L331-350、`deviceAct` L375-392、`buildLightCard` の card record L315-322、`POLL_MS` 付近 L194 に定数追加）

**Interfaces:**
- Consumes: Task 1 の light POST 応答形 `{"action":"success"|...}`（`state` キーなし）。shutter POST 応答形は従来どおり。
- Produces: `lightAct(name, verb, path)` と `scheduleLightCatchup(name)`（内部関数。後続タスクからの依存なし）。

- [ ] **Step 1: light 操作パスを実装**

`index.html` を以下のとおり編集する。JS の自動テストは無いプロジェクトなので、このタスクはビルド確認 + Task 4 の実機確認で検証する。

(a) L194 `const POLL_MS = 4000;` の直後に追加:

```js
const LIGHT_CATCHUP_MS = 2000; // light: 操作後に 1 回だけ state を追いつき取得するまでの待ち。
```

(b) `buildLightCard`（L315-322）の card record に `catchupTimer` を追加:

```js
  const c = {
    kind: "light",
    statusEl: el.querySelector(".status"),
    labelEl: el.querySelector(".status .label"),
    msgEl: el.querySelector(".status .msg"),
    buttons: [...el.querySelectorAll("button")], // act + chip 全部を busy 対象に
    state: "unknown",
    catchupTimer: null, // 追いつき取得タイマー（device ごとに 1 本、連打時は張り直し）
  };
```

(c) `presetAct`（L331-350）全体を以下で置き換える:

```js
/* ── プリセット実行（light 専用）───────────────────── */
async function presetAct(name, preset) {
  return lightAct(
    name,
    VERB.preset,
    `/api/devices/${encodeURIComponent(name)}/presets/${encodeURIComponent(preset)}`
  );
}

/* ── light 操作: POST は送信結果のみ。state は ~2 秒後に 1 回だけ追いつき取得 ── */
async function lightAct(name, verb, path) {
  const c = cards.get(name);
  busyCount++;
  setDeviceBusy(name, true);
  if (c) c.msgEl.textContent = verb;
  try {
    const view = await api("POST", path);
    const am = ACTION_MSG[view.action] || "";
    if (am) {
      c.msgEl.textContent = "⚠ " + am;
      c.statusEl.classList.add("error");
    } else {
      // 成功 = 送信できただけ。中間表示にして追いつき取得を予約する。
      c.statusEl.classList.remove("error");
      c.msgEl.textContent = "";
      c.labelEl.textContent = "反映中…";
      scheduleLightCatchup(name);
    }
  } catch (e) {
    if (c) { c.msgEl.textContent = "⚠ 通信エラー"; c.statusEl.classList.add("error"); }
  } finally {
    setDeviceBusy(name, false);
    busyCount--;
  }
}

function scheduleLightCatchup(name) {
  const c = cards.get(name);
  if (!c) return;
  if (c.catchupTimer) clearTimeout(c.catchupTimer); // 連打時は張り直し
  c.catchupTimer = setTimeout(async () => {
    c.catchupTimer = null;
    // busy（連打中・一括操作中）なら取得をスキップ。ベストエフォート表示でよい。
    if (c.buttons.some((b) => b.disabled)) return;
    try {
      const view = await api("GET", `/api/devices/${encodeURIComponent(name)}/state`);
      renderState(name, view);
    } catch (e) {
      // 外れていても次の操作 or ページ再表示で直る。
    }
  }, LIGHT_CATCHUP_MS);
}
```

(d) `deviceAct`（L375-392）の冒頭に light 分岐を足す（shutter パスは無変更）:

```js
/* ── 単一デバイス操作 ───────────────────────────── */
async function deviceAct(name, op) {
  const c = cards.get(name);
  if (c && c.kind === "light") {
    return lightAct(name, VERB[op] || "実行中…", `/api/devices/${encodeURIComponent(name)}/${op}`);
  }
  busyCount++;
  setDeviceBusy(name, true);
  if (c) c.msgEl.textContent = VERB[op] || "実行中…";
  try {
    const view = await api("POST", `/api/devices/${encodeURIComponent(name)}/${op}`);
    renderState(name, view);
    const am = ACTION_MSG[view.action] || "";
    if (am) { c.msgEl.textContent = "⚠ " + am; c.statusEl.classList.add("error"); }
  } catch (e) {
    if (c) { c.msgEl.textContent = "⚠ 通信エラー"; c.statusEl.classList.add("error"); }
  } finally {
    setDeviceBusy(name, false);
    busyCount--;
    updateGroupSummaries();
  }
}
```

（元の関数本体と同一。追加は冒頭 4 行の light 分岐のみ。`const c` の重複宣言に注意 — 元の `const c = cards.get(name);` は冒頭に移動済みなので、try ブロック前の再宣言は削除する。）

- [ ] **Step 2: ビルド確認（index.html は include_str! で焼き込まれる）**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 PASS・警告ゼロ（JS は Rust テスト対象外だが、焼き込みビルドが通ることを確認）。

- [ ] **Step 3: ローカルで目視スモークテスト**

config.toml が sh 偽装でよいので、テスト用 config で起動して light カードの挙動を確認する:

```bash
cat > /tmp/mando-smoke.toml <<'EOF'
bind = "127.0.0.1:18080"
[[device]]
name = "light"
alias = "テスト照明"
kind = "light"
get_state = ["sh", "-c", "sleep 0.3; printf '{\"value\":true}'"]
on  = ["sh", "-c", "sleep 1; printf '{}'"]
off = ["sh", "-c", "sleep 1; printf '{}'"]
EOF
MANDO_CONFIG=/tmp/mando-smoke.toml cargo run &
sleep 3
# POST が state 抜きで即返ること（~1 秒、{"action":"success"} のみ）:
time curl -s -X POST http://127.0.0.1:18080/api/devices/light/on
kill %1
```

Expected: POST 応答が `{"action":"success"}`（`state` キーなし）で、所要 ~1 秒（sleep 1 のみ。従来は +0.3 秒の state 読みが乗っていた）。ブラウザ確認は Task 4 の実機で行う。

- [ ] **Step 4: Commit**

```bash
git add index.html
git commit -m "feat: light UI はボタン即解放 + 2 秒後に state を追いつき取得"
```

---

### Task 3: docs — 設計原則 7 の適用範囲明文化 + config サンプル更新

**Files:**
- Modify: `CLAUDE.md`(原則 7 の表の直後)
- Modify: `config.example.toml`(L74-87 のコメント)

**Interfaces:**
- Consumes: なし（文書のみ）
- Produces: なし

- [ ] **Step 1: CLAUDE.md 原則 7 に light 例外の注記を追加**

原則 7 の終了コード表の直後（`8. **\`index.html\` はバイナリに焼く` の前）に以下を挿入:

```markdown
   > **light の例外:** light（特に mat wire group の groupcast）は無応答マルチキャストで、
   > 確認読み自体が代表ノード 1 台のプロキシ読みにすぎず確認として弱い。操作 POST は
   > 送信結果（`{"action": ...}`）のみ正直に返し、state は UI が押下 ~2 秒後に 1 回だけ
   > 非同期で追いつき取得するベストエフォート表示とする
   > （`docs/superpowers/specs/2026-07-10-light-async-state-design.md`）。
   > shutter は本原則どおり set 後の同期確認を維持する。
```

- [ ] **Step 2: config.example.toml のコメントを本設計に合わせる**

L75-76 の

```
# 注意: light は定期ポーリングしない（mat 直叩きは 1 コール数秒 + exec 直列のため）。
#       表示時 1 回 + 操作後の確定表示のみ。
```

を以下に置き換え:

```
# 注意: light は定期ポーリングしない（mat 直叩きは 1 コール数秒 + exec 直列のため）。
#       表示時 1 回 + 操作の ~2 秒後に 1 回の追いつき取得のみ（ベストエフォート表示）。
```

L78-87 の wire group ブロックを以下に置き換え（sh ラッパー推奨を撤去し、素の `mat read` に）:

```
# ── mat の wire group（groupcast）でまとめて操作する場合 ──────────────
# group は無応答マルチキャストで state を読めないため、get_state は代表ノード
# 1 台の on/off を読む。操作 POST は送信結果のみ返し、state は UI が ~2 秒後に
# 1 回だけ追いつき取得するので、確認読みを sh ラッパーで遅らせる必要はない。
# 応答を速くするには matd を常駐させる（mando の unit に MAT_MATD_SOCKET を
# 教える。matd 無しだと 1 コールごとに chip-tool 起動 + CASE 確立で数秒かかる）。
# get_state = ["mat", "read", "--node", "5", "--cluster", "onoff", "--attribute", "on-off"]
# on    = ["mat", "group", "invoke", "--group", "10", "--cluster", "onoff", "--command", "on"]
# off   = ["mat", "group", "invoke", "--group", "10", "--cluster", "onoff", "--command", "off"]
# （preset も同様に ["mat", "group", "color-temp", "--group", "10", "--kelvin", "2700"] 等）
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md config.example.toml
git commit -m "docs: 設計原則 7 に light 例外を明文化、config 例から sleep ラッパーを撤去"
```

---

### Task 4: デプロイ + jarvis config 修正 + e2e 検証

**メインセッションで実施する**（実機 ssh + sudo を伴う。spec でユーザー承認済み）。subagent には出さない。

**Files:**
- リポジトリ変更なし。jarvis 上の `/etc/mando/config.toml` を編集（リポジトリ外の実 config）。

- [ ] **Step 1: デプロイ（クロスビルド → scp → 再起動）**

Run: `task deploy`
Expected: `cross build --release --target aarch64-unknown-linux-musl` 成功 → scp → `systemctl restart mando` → status が active。

- [ ] **Step 2: jarvis の実 config から sleep ラッパーを撤去**

現状確認:

```bash
ssh jarvis 'sudo grep -n "get_state" /etc/mando/config.toml'
```

`["sh", "-c", "sleep 0.5; exec mat read ..."]` 型の行を、素の `mat read` に書き換える（node 番号等は現物の値を維持）:

```bash
ssh jarvis 'sudo sed -i \
  "s|\[\"sh\", \"-c\", \"sleep 0.5; exec mat read \(.*\)\"\]|[\"mat\", \"read\", \1]|" \
  /etc/mando/config.toml'
```

> sed が現物の書式と合わなければ、`sudo cat` で確認して手で正確に置換する
> （引数はカンマ区切りの配列に戻す: `["mat", "read", "--node", "5", "--cluster", "onoff", "--attribute", "on-off"]`）。

反映: `ssh jarvis 'sudo systemctl restart mando && systemctl status mando --no-pager --lines=0'`
Expected: active (running)。

- [ ] **Step 3: e2e 検証（curl）**

light デバイス名と bind ポートを確認してから:

```bash
curl -s http://jarvis:8080/api/devices | python3 -m json.tool   # light の name を確認
time curl -s -X POST http://jarvis:8080/api/devices/<light名>/on
```

Expected: ~1 秒前後で `{"action":"success"}`（`state` キーなし）。

```bash
sleep 3
curl -s http://jarvis:8080/api/devices/<light名>/state
```

Expected: `"state":"on"` に追いついている。

復元:

```bash
curl -s -X POST http://jarvis:8080/api/devices/<light名>/off
sleep 3
curl -s http://jarvis:8080/api/devices/<light名>/state   # "off" を確認
```

- [ ] **Step 4: UI 実機確認**

スマホ or ブラウザで `http://jarvis:8080/` を開き:
- light の点/消 押下 → **~1 秒でボタン再有効**（従来 ~2.3 秒）
- 状態ラベルが「反映中…」→ 数秒後に「点灯/消灯」へ追いつく
- 連打してもエラーにならない（追いつき取得はスキップされ、最後の操作の 2 秒後に 1 回）
- shutter の開/閉/停とポーリングが従来どおり動く

- [ ] **Step 5: push**

```bash
git push
```

---

## Self-Review（実施済み）

- **Spec coverage:** 決定 1（POST は exec 結果のみ）→ Task 1。決定 2（UI 追いつき取得）→ Task 2。決定 3（shutter 不変）→ Task 1 Step 1 の回帰テスト + Global Constraints。決定 4（CLAUDE.md 注記）→ Task 3。決定 5（jarvis sleep ラッパー撤去 + example 更新）→ Task 3 / Task 4。API 変更点・テスト/検証・やらないこと、すべて対応タスクあり。
- **Placeholder scan:** なし（全ステップに実コード・実コマンド・期待出力あり）。
- **Type consistency:** `LightActionView` / `run_light_action` / `lightAct` / `scheduleLightCatchup` / `LIGHT_CATCHUP_MS` / `catchupTimer` の名前は各タスク間で一致。
