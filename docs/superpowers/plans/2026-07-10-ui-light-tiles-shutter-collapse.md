# 照明タイル + シャッター個別折りたたみ UI 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** UI を「💡 照明（タイル + 電球ボタン）」「🪟 シャッター（一括カード + 個別折りたたみ）」の 2 セクションに再構成する。

**Architecture:** サーバ変更は preset への任意 `color` フィールド追加（config → API 露出）のみ。残りはすべて `index.html`（焼き込み UI）の CSS/JS。既存の `cards` マップ / `renderState` / `lightAct` / 追いつき取得の機構は無改修で流用し、DOM の組み立て（boot / build 系関数）と CSS だけを差し替える。

**Tech Stack:** Rust (axum, serde), vanilla JS + CSS（`index.html` に同梱、ビルド工程なし）。

**Spec:** `docs/superpowers/specs/2026-07-10-ui-light-tiles-shutter-collapse-design.md`

## Global Constraints

- 設計原則 7: light の操作 POST は送信結果のみ・state は ~2 秒後の追いつき取得（既存挙動を変えない）。shutter は set 後同期確認のまま。
- API・エンドポイント・exec 経路は preset `color` の露出以外一切変えない。
- `cargo test` 全通過 + `cargo clippy -- -D warnings` クリーンを各コミット前に確認。
- コミットメッセージは日本語・conventional commits（既存ログ参照）。
- UI 文言は日本語（既存の STATE_LABEL / VERB / ACTION_MSG に合わせる）。

---

### Task 1: preset の `color` を config → API に通す

**Files:**
- Modify: `src/config.rs:21-30`（`Preset` 構造体）、`:422-454`（`light_device_parses` テスト）
- Modify: `src/main.rs:112-116`（`PresetInfo`）、`:139-146`（`list_devices`）、`:471-527`（テスト fixture と `devices_list_has_kind_and_presets`）
- Modify: `config.example.toml:58-71`（preset 例に color を追記）

**Interfaces:**
- Produces: `GET /api/devices` の `presets[]` 各要素に `color: string | null` が追加される（Task 3 の UI が読む）。config の `[[device.preset]]` に任意キー `color`（CSS color 文字列、検証なし）。

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `light_device_parses` テスト（424 行付近）の fixture 先頭 preset に `color` を足し、末尾の assert 2 行の後に color の検証を追加:

```rust
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            color = "#ffd9a0"
            cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]
```

```rust
        assert_eq!(d.presets[0].color.as_deref(), Some("#ffd9a0"));
        assert_eq!(d.presets[1].color, None); // color 未指定は None
```

`src/main.rs` のテスト fixture（480 行付近の `[[device.preset]]`）に `color = "#ffd9a0"` を 1 行追加:

```toml
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            color = "#ffd9a0"
            cmd   = ["sh", "-c", "printf '{}'"]
```

`devices_list_has_kind_and_presets`（523 行付近）の label assert の後に追加:

```rust
        assert_eq!(light["presets"][0]["color"], "#ffd9a0");
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test light_device_parses devices_list_has_kind_and_presets`
Expected: コンパイルエラー（`Preset` に `color` フィールドがない）。これも「失敗の確認」として扱う。

- [ ] **Step 3: 最小実装**

`src/config.rs` の `Preset`（22 行付近）にフィールド追加:

```rust
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
```

`src/main.rs` の `PresetInfo`（112 行付近）と `list_devices` の写し（142 行付近）:

```rust
#[derive(Serialize)]
struct PresetInfo {
    name: String,
    label: String,
    /// 色玉スウォッチ用 CSS color。None なら UI はテキストチップで出す。
    color: Option<String>,
}
```

```rust
                .map(|p| PresetInfo {
                    name: p.name.clone(),
                    label: p.label().to_string(),
                    color: p.color.clone(),
                })
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全テスト PASS、clippy 警告なし。

- [ ] **Step 5: config.example.toml に color の例と注記を追加**

`config.example.toml` の preset 例（58-71 行付近）を次に置き換え（`color` 行の追加とチップ注記の更新のみ）:

```toml
# kind = "light" は on / off / get_state 必須。色・色温度は [[device.preset]] に
# 完成済みコマンドを並べる（任意値入力は作らない）。color（CSS color 文字列）を
# 書くと UI が色玉スウォッチで出す。省略するとテキストチップになる。
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
# color = "#ffd9a0"
# cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]
#
# [[device.preset]]
# name  = "daylight"
# label = "白色"
# color = "#f2f5ff"
# cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "5000"]
#
# [[device.preset]]
# name  = "pink"
# label = "ピンク"
# color = "#ff69b4"
# cmd   = ["mat", "color", "--node", "5", "--name", "pink"]
```

- [ ] **Step 6: コミット**

```bash
git add src/config.rs src/main.rs config.example.toml
git commit -m "feat: preset に色玉スウォッチ用の任意 color を追加"
```

---

### Task 2: セクション分け + シャッター個別の折りたたみ（index.html）

**Files:**
- Modify: `index.html`（CSS: `.group` の sticky 撤去 + セクション/行スタイル追加。JS: `buildGroup` / `buildCard` はそのまま、`buildMemberRow` / `sectionHeading` 追加、`boot` 再構成）

**Interfaces:**
- Consumes: 既存 `cards` マップの要素形 `{statusEl, labelEl, msgEl, buttons, kind, state}`（`renderState` / `setDeviceBusy` / `updateGroupSummaries` / `setAllButtonsDisabled` が読む）。
- Produces: `sectionHeading(text)`、`buildMemberRow(dev)`、`buildGroup(g, byName)`（第 2 引数追加）。Task 3 は `boot()` の照明セクション部だけを差し替える。

- [ ] **Step 1: CSS — sticky 撤去とセクション/展開/行スタイル追加**

`.group` のルール（65-72 行付近）から `position: sticky; top: 8px; z-index: 5;` を削除:

```css
  .group {
    background: var(--panel2); border: 1px solid var(--line2);
    border-radius: 20px; padding: 14px 16px; margin: 8px 0 16px;
    box-shadow: 0 10px 30px rgba(0,0,0,.42), inset 0 1px 0 rgba(255,255,255,.05);
    backdrop-filter: blur(16px) saturate(140%);
    -webkit-backdrop-filter: blur(16px) saturate(140%);
  }
```

`.boot` ルールの直前（159 行付近）に追加:

```css
  /* ── セクション見出し ─────────────────────────── */
  .sec {
    font-size: 12px; color: var(--muted); font-weight: 700;
    letter-spacing: .08em; margin: 18px 4px 2px;
  }
  .sec:first-child { margin-top: 6px; }

  /* ── シャッター個別の折りたたみ ─────────────────── */
  button.expander {
    appearance: none; width: 100%; margin-top: 11px; padding: 9px;
    background: none; border: none; border-top: 1px solid var(--line);
    color: var(--muted); font-size: 12px; font-weight: 700; cursor: pointer;
    touch-action: manipulation; user-select: none;
  }
  button.expander:active { color: var(--fg); }
  .drow {
    display: flex; align-items: center; gap: 10px;
    border-top: 1px solid var(--line); padding: 9px 2px;
  }
  .drow .dinfo { flex: 1; min-width: 0; }
  .drow .dname { font-size: 13px; font-weight: 700; }
  .drow .status { margin-top: 3px; min-height: 16px; }
  .drow button.act { width: 46px; min-height: 44px; font-size: 15px; border-radius: 12px; }
```

- [ ] **Step 2: JS — buildGroup に展開部を追加（メンバー行つき）**

`buildGroup` を次に置き換え（`byName` 引数・expander・members が追加点。group 操作ボタンのセレクタを `.gctl button.act` に絞るのが重要 — member 行の `.act` を誤って拾わないため）:

```js
/* ── 一括グループバー ───────────────────────────── */
function buildGroup(g, byName) {
  const el = document.createElement("div");
  el.className = "group";
  el.innerHTML = `
    <div class="ghead">
      <span class="gname"></span>
      <span class="gsum"></span>
    </div>
    <div class="gctl">
      <button class="act open" data-op="open">開ける</button>
      <button class="act close" data-op="close">閉める</button>
    </div>
    <div class="gbusy"></div>
    <button class="expander" type="button">個別に操作 ▾</button>
    <div class="members" hidden></div>
  `;
  el.querySelector(".gname").textContent = g.label + " 一括";
  if (g.stop) {
    const b = document.createElement("button");
    b.className = "act stop"; b.dataset.op = "stop"; b.textContent = "止める";
    el.querySelector(".gctl").appendChild(b);
  }
  const membersEl = el.querySelector(".members");
  for (const m of g.members) {
    const dev = byName.get(m);
    if (dev) membersEl.appendChild(buildMemberRow(dev));
  }
  const exp = el.querySelector(".expander");
  exp.addEventListener("click", () => {
    membersEl.hidden = !membersEl.hidden;
    exp.textContent = membersEl.hidden ? "個別に操作 ▾" : "個別に操作 ▴";
  });
  const buttons = [...el.querySelectorAll(".gctl button.act")];
  const rec = {
    name: g.name, el, buttons, members: g.members,
    busyEl: el.querySelector(".gbusy"),
    summaryEl: el.querySelector(".gsum"),
  };
  for (const btn of buttons) btn.addEventListener("click", () => groupAct(rec, btn.dataset.op));
  groups.push(rec);
  return el;
}
```

- [ ] **Step 3: JS — buildMemberRow と sectionHeading を追加**

`buildCard` の直前に追加。`cards` への登録形は既存カードと同一なので `renderState` / busy 制御 / 一括中の全無効化がそのまま効く:

```js
/* ── グループ内の個別シャッター行（折りたたみ内）───── */
function buildMemberRow(dev) {
  const el = document.createElement("div");
  el.className = "drow";
  el.innerHTML = `
    <div class="dinfo">
      <div class="dname"></div>
      <div class="status unknown"><span class="dot"></span><span class="label">不明</span><span class="msg"></span></div>
    </div>
    <button class="act open" data-op="open">開</button>
    <button class="act close" data-op="close">閉</button>
  `;
  el.querySelector(".dname").textContent = dev.label;
  if (dev.stop) {
    const b = document.createElement("button");
    b.className = "act stop"; b.dataset.op = "stop"; b.textContent = "停";
    el.appendChild(b);
  }
  const c = {
    statusEl: el.querySelector(".status"),
    labelEl: el.querySelector(".status .label"),
    msgEl: el.querySelector(".status .msg"),
    buttons: [...el.querySelectorAll("button.act")],
    kind: "shutter",
    state: "unknown",
  };
  for (const btn of c.buttons) btn.addEventListener("click", () => deviceAct(dev.name, btn.dataset.op));
  cards.set(dev.name, c);
  return el;
}

function sectionHeading(text) {
  const el = document.createElement("div");
  el.className = "sec";
  el.textContent = text;
  return el;
}
```

- [ ] **Step 4: JS — boot をセクション構成に再構成**

`boot()` の `app.innerHTML = "";` 以降のカード構築部を次に置き換え（この Task では light はまだ `buildLightCard` のまま）:

```js
  app.innerHTML = "";
  const byName = new Map(devices.map((d) => [d.name, d]));
  const lights = devices.filter((d) => d.kind === "light");
  const shutters = devices.filter((d) => d.kind !== "light");
  const grouped = new Set(grps.flatMap((g) => g.members));

  if (lights.length) {
    app.appendChild(sectionHeading("💡 照明"));
    for (const dev of lights) app.appendChild(buildLightCard(dev));
  }
  if (shutters.length) {
    app.appendChild(sectionHeading("🪟 シャッター"));
    for (const g of grps) app.appendChild(buildGroup(g, byName));
    // グループ非所属の shutter は従来カードで並べる（グループ所属は行で登録済み）。
    for (const dev of shutters) {
      if (!grouped.has(dev.name)) app.appendChild(buildCard(dev));
    }
  }
  startPolling();
  fetchLightStatesOnce(devices);
```

- [ ] **Step 5: JS 構文チェックとビルド**

```bash
sed -n '/^<script>$/,/^<\/script>$/p' index.html | sed '1d;$d' > /tmp/mando-ui.js && node --check /tmp/mando-ui.js && cargo build
```

Expected: node のエラーなし、ビルド成功（`include_str!` で index.html が焼き込まれる）。

- [ ] **Step 6: 偽 config での目視スモーク**

リポジトリ外（例 `/tmp/mando-smoke.toml`）に sh 偽装 config を作って起動:

```toml
bind = "127.0.0.1:8899"

[[device]]
name  = "living"
alias = "リビング"
kind  = "light"
get_state = ["sh", "-c", "printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]

[[device.preset]]
name  = "warm"
label = "電球色"
color = "#ffd9a0"
cmd   = ["sh", "-c", "printf '{}'"]

[[device.preset]]
name  = "plain"
label = "白色"
cmd   = ["sh", "-c", "printf '{}'"]

[[device]]
name  = "minami"
alias = "南の窓"
get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
open  = ["sh", "-c", "printf '{}'"]
close = ["sh", "-c", "printf '{}'"]

[[device]]
name  = "higashi"
alias = "東の窓"
get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"closed\"}]}'"]
open  = ["sh", "-c", "printf '{}'"]
close = ["sh", "-c", "printf '{}'"]

[[group]]
name    = "all"
alias   = "シャッター"
members = ["minami", "higashi"]
```

```bash
MANDO_CONFIG=/tmp/mando-smoke.toml cargo run &
sleep 2
curl -s http://127.0.0.1:8899/api/devices | python3 -m json.tool   # color が出ること
curl -s http://127.0.0.1:8899/ | grep -c "個別に操作"               # 1 以上
kill %1
```

Expected: devices JSON に `"color": "#ffd9a0"` と `"color": null`、HTML に展開ボタン文言。DOM 挙動（展開・操作）はブラウザで開ける環境なら確認、なければ Task 4 の実機確認に委ねる。

あわせて偽 config から `[[device]]`（shutter 2 台と group）を消した light-only 版でも起動し、
ブラウザ確認できる場合は「🪟 シャッター」見出しが出ないこと（逆に shutter-only なら「💡 照明」が
出ないこと）を確認する。確認できない環境ではコードレビューで `if (lights.length)` /
`if (shutters.length)` の分岐を確認すれば足りる。

- [ ] **Step 7: コミット**

```bash
git add index.html
git commit -m "feat: UI をセクション分けし、シャッター個別操作を一括カード内に折りたたみ"
```

---

### Task 3: ライトタイル（電球ボタン + 色玉スウォッチ）（index.html）

**Files:**
- Modify: `index.html`（CSS: `--lamp` 変数 + タイル/電球/スウォッチ、`.presets` ブロック削除。JS: `buildLightCard` → `buildLightTile` 置換、`renderState` に lit トグル追加、`boot` の照明部差し替え）

**Interfaces:**
- Consumes: Task 1 の `presets[].color`、Task 2 の `sectionHeading` と boot 構成。既存 `deviceAct` / `presetAct` / `lightAct` / `scheduleLightCatchup`（無改修）。
- Produces: `buildLightTile(dev)`。`cards` の light 要素に `rootEl` が追加され、`renderState` が `.lit` クラスをトグルする。

- [ ] **Step 1: CSS — タイル/電球/スウォッチを追加、旧 .presets を削除**

`:root` に変数追加（`--closed2` の行の後）:

```css
    --lamp: #ffb84d;
    --lamp2: #e8951f;
```

「── light プリセットチップ ──」ブロック（115-117 行付近の `.presets` 2 ルール）を削除し、同じ場所に追加（`button.chip` のルールは残す — 色なし preset のフォールバックで使う）:

```css
  /* ── light タイル（2 列グリッド）────────────────── */
  .tiles { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin: 10px 0; }
  .tile {
    border-radius: 16px; padding: 13px;
    background: var(--panel); border: 1px solid var(--line);
    box-shadow: 0 4px 16px rgba(0,0,0,.28), inset 0 1px 0 rgba(255,255,255,.04);
    backdrop-filter: blur(10px); -webkit-backdrop-filter: blur(10px);
    display: flex; flex-direction: column; gap: 4px;
    animation: rise .35s cubic-bezier(.2,.8,.3,1) both;
    transition: border-color var(--tap), box-shadow var(--tap);
  }
  .tile.lit {
    background: linear-gradient(135deg, rgba(255,184,77,.16), var(--panel) 62%);
    border-color: rgba(255,184,77,.30);
    box-shadow: 0 0 22px rgba(255,184,77,.10), 0 4px 16px rgba(0,0,0,.28);
  }
  button.bulb {
    appearance: none; width: 66px; height: 66px; border-radius: 50%;
    border: 1px solid var(--line2); font-size: 30px; align-self: center;
    margin: 2px 0 4px; padding: 0; cursor: pointer;
    background: rgba(255,255,255,.04); filter: grayscale(1); opacity: .55;
    touch-action: manipulation; user-select: none;
    transition: transform var(--tap), filter var(--tap), opacity var(--tap), box-shadow var(--tap);
  }
  .tile.lit button.bulb {
    border-color: transparent; filter: none; opacity: 1;
    background: radial-gradient(circle at 50% 38%, rgba(255,214,140,.45), rgba(255,184,77,.10) 72%);
    box-shadow: 0 0 22px rgba(255,184,77,.35), inset 0 1px 0 rgba(255,255,255,.2);
  }
  button.bulb:active:not(:disabled) { transform: scale(.93); }
  button.bulb:disabled { opacity: .35; cursor: progress; }
  .tile .tname { font-size: 14px; font-weight: 700; text-align: center; }
  .tile .status { justify-content: center; margin-top: 2px; min-height: 16px; }
  .tile.lit .status .label { color: #ffca7a; }
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
```

- [ ] **Step 2: JS — buildLightCard を buildLightTile に置き換え**

`buildLightCard` 関数全体（293-330 行付近）を次に置き換え。電球押下は表示中 state の反対を送る（`on` なら off、`off`/`unknown` なら on）。スウォッチのリングは「このセッションで最後に押した色」の目印で、現在色の主張ではない:

```js
/* ── light タイル（電球ボタン = スイッチ + 色玉）──── */
function buildLightTile(dev) {
  const el = document.createElement("div");
  el.className = "tile";
  el.innerHTML = `
    <button class="bulb" type="button" aria-label="点灯/消灯">💡</button>
    <div class="tname"></div>
    <div class="status unknown"><span class="label">不明</span><span class="msg"></span></div>
    <div class="swatches"></div>
  `;
  el.querySelector(".tname").textContent = dev.label;
  const sw = el.querySelector(".swatches");
  for (const p of dev.presets) {
    const b = document.createElement("button");
    if (p.color) {
      b.className = "sw";
      b.style.background = p.color;
      b.title = p.label;
      b.setAttribute("aria-label", p.label);
    } else {
      b.className = "chip";
      b.textContent = p.label;
    }
    b.addEventListener("click", () => {
      // リングは「最後に押した色」の目印。現在色の主張はしない（原則 7）。
      for (const s of sw.children) s.classList.remove("sel");
      b.classList.add("sel");
      presetAct(dev.name, p.name);
    });
    sw.appendChild(b);
  }
  const c = {
    kind: "light",
    rootEl: el,
    statusEl: el.querySelector(".status"),
    labelEl: el.querySelector(".status .label"),
    msgEl: el.querySelector(".status .msg"),
    buttons: [...el.querySelectorAll("button")], // bulb + sw/chip 全部を busy 対象に
    state: "unknown",
    catchupTimer: null, // 追いつき取得タイマー（device ごとに 1 本、連打時は張り直し）
  };
  el.querySelector(".bulb").addEventListener("click", () =>
    deviceAct(dev.name, c.state === "on" ? "off" : "on")
  );
  cards.set(dev.name, c);
  return el;
}
```

- [ ] **Step 3: JS — renderState に lit トグルを追加、boot の照明部をタイルに**

`renderState` の `c.labelEl.textContent = ...` の直後に 1 行追加:

```js
  if (c.rootEl) c.rootEl.classList.toggle("lit", st === "on");
```

`boot()` の照明セクション部を差し替え:

```js
  if (lights.length) {
    app.appendChild(sectionHeading("💡 照明"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of lights) tiles.appendChild(buildLightTile(dev));
    app.appendChild(tiles);
  }
```

- [ ] **Step 4: 構文チェック + テスト + スモーク**

```bash
sed -n '/^<script>$/,/^<\/script>$/p' index.html | sed '1d;$d' > /tmp/mando-ui.js && node --check /tmp/mando-ui.js
cargo test && cargo clippy -- -D warnings
```

Expected: すべて成功。さらに Task 2 Step 6 と同じ偽 config スモークで:

```bash
MANDO_CONFIG=/tmp/mando-smoke.toml cargo run &
sleep 2
curl -s http://127.0.0.1:8899/ | grep -c "buildLightTile"   # 1 以上（関数が焼き込まれている）
curl -s -X POST http://127.0.0.1:8899/api/devices/living/on # {"action":"success"}
kill %1
```

- [ ] **Step 5: コミット**

```bash
git add index.html
git commit -m "feat: light をタイル表示にし電球ボタンをトグルスイッチ化、色玉スウォッチ対応"
```

---

### Task 4: デプロイと実機確認

**Files:**
- なし（デプロイ作業のみ。jarvis の実 config に preset `color` を足すのは任意 — なくてもチップ表示で動く）

**Interfaces:**
- Consumes: Task 1-3 の成果一式（単一バイナリ）。

- [ ] **Step 1: 全体検証**

```bash
cargo test && cargo clippy -- -D warnings && cargo build --release
```

Expected: すべて成功。

- [ ] **Step 2: jarvis の実 config に色玉の color を追記（ssh）**

`ssh jarvis` して `/etc/mando/config.toml` の各 `[[device.preset]]` に `color` を追加（sudo 要）。
実機の preset 構成に合わせるが、既知の目安: 電球色 `#ffd9a0` / 白色 `#f2f5ff` / ピンク `#ff69b4`
（メモリ: jarvis の mat は pink=#ff69b4 に上書き済み）。編集後は Step 3 の deploy が再起動を担うので
ここでは再起動不要。追記せずチップ表示のまま先に進んでもよい。

- [ ] **Step 3: デプロイ**

```bash
task deploy
```

Expected: クロスビルド → jarvis へ転送 → 再起動が成功（Taskfile の deploy タスク、HOST デフォルト jarvis）。

- [ ] **Step 4: 実機確認（ユーザーに依頼）**

スマホで確認してもらう項目:
- 照明セクション: タイル表示・電球押下で点/消（~2 秒後に表示が追いつく）・色玉で色変更 + リング移動
- シャッターセクション: 初期は一括カードのみ → 「個別に操作 ▾」で展開 → 個別の開/閉/停が動く → 一括操作中は全ボタン無効
- 途中で jarvis のモック配信サーバ（`/tmp/mando-mock`, port 8901）が残っていれば停止:

```bash
ssh jarvis 'pkill -f "http.server 8901"'
```

- [ ] **Step 5: 完了処理**

実機確認 OK をもらったら superpowers:finishing-a-development-branch に従って締める（main 直コミット運用なので push の要否をユーザーに確認）。
