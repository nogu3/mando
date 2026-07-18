# アクティブ窓ポーリング Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** シャッター state の常時 4 秒ポーリングを廃止し、表示時 1 回 + 操作後 2 分（動作中は延長、上限 10 分）の「アクティブ窓」の間だけポーリングする。

**Architecture:** `index.html` 内の JS のみ変更。`pollUntil`（窓の期限）と `pollAnchor`（直近トリガー時刻、延長上限の基準）を持ち、期限つきループが `now < pollUntil` の間だけ 4 秒間隔で回る。窓の外ではループ自体が終了し enl 実行ゼロ。サーバ（Rust）側・ライトの既存動作（表示時 1 回 + 操作 2 秒後の追いつき）は変更しない。

**Tech Stack:** Vanilla JS（`index.html` に焼き込み）、Rust/axum サーバは無変更。

**Spec:** `docs/superpowers/specs/2026-07-18-active-window-polling-design.md`

## Global Constraints

- 変更ファイルは `index.html` と `CLAUDE.md` のみ。`src/` は触らない。
- 窓の外ではポーリング完全停止（enl 実行ゼロ）。
- ページ表示 / タブ復帰は state を 1 回だけ取得し、窓は開かない。
- シャッター操作（個別 open/close/stop・一括とも）完了後は「今から 2 分」の窓。
- `opening` / `closing` が見えている間は「今から 30 秒」まで延長。ただし直近トリガー（操作 or 表示）から 10 分を上限とする。
- state 取得エラーでは窓を延長しない。`busyCount > 0` 中のポーリングスキップは維持。
- JS テスト基盤はリポジトリに無い（テストは Rust の `cargo test` のみ）。新設しない。検証は `cargo build && cargo test && cargo clippy -- -D warnings` + ブラウザ/サーバログでの動作確認。
- コミットメッセージは日本語 Conventional Commits（例: `feat: ...を追加`）。

---

### Task 1: ポーリング窓機構の導入（常時ループの置き換え）

**Files:**
- Modify: `index.html:358`（定数）
- Modify: `index.html:364`（状態変数 `let polling = false;` を置き換え）
- Modify: `index.html:844-866`（`pollOnce` 末尾に延長判定、`startPolling` を窓機構に置き換え）
- Modify: `index.html:1192`（boot 内 `startPolling()` → `refreshOnce()`）
- Modify: `index.html:1196`（visibilitychange → `refreshOnce()`）

**Interfaces:**
- Consumes: 既存の `pollOnce()`（busyCount ガード・light スキップ入り）、`cards` Map（各エントリに `kind` と `state`）。
- Produces: `bumpPollWindow(ms)` — トリガー起点で窓を開く（Task 2 が操作完了時に `bumpPollWindow(POLL_WINDOW_MS)` として呼ぶ）。`extendPollWindow(ms)`、`ensurePollLoop()`、`refreshOnce()`、定数 `POLL_WINDOW_MS` / `POLL_EXTEND_MS` / `POLL_EXTEND_CAP_MS` / `MOVING_STATES`。

- [ ] **Step 1: 定数を差し替える**

`index.html:358` の

```js
const POLL_MS = 4000; // 3〜5 秒ポーリング（状態は pull）。
```

を以下に置き換える:

```js
const POLL_MS = 4000; // アクティブ窓の間のポーリング間隔。
const POLL_WINDOW_MS = 2 * 60 * 1000;      // 操作後にポーリングを続ける窓。
const POLL_EXTEND_MS = 30 * 1000;          // 動作中（opening/closing）が見えている間の延長単位。
const POLL_EXTEND_CAP_MS = 10 * 60 * 1000; // 直近トリガーからの延長上限（固着時の無限ポーリング防止）。
const MOVING_STATES = new Set(["opening", "closing"]);
```

- [ ] **Step 2: 状態変数を差し替える**

`index.html:364` の

```js
let polling = false;
```

を以下に置き換える:

```js
let pollUntil = 0;          // この時刻までポーリング（アクティブ窓）。窓の外は完全停止。
let pollAnchor = 0;         // 直近トリガー（操作 / 表示）時刻。延長上限の基準。
let pollLoopRunning = false;
```

- [ ] **Step 3: `startPolling` を窓機構に置き換える**

`index.html:861-866` の

```js
function startPolling() {
  if (polling) return;
  polling = true;
  const tick = async () => { await pollOnce(); setTimeout(tick, POLL_MS); };
  tick();
}
```

を以下に置き換える:

```js
/* 窓を「今から ms」まで延ばす（短縮はしない）。上限は直近トリガーから POLL_EXTEND_CAP_MS。 */
function extendPollWindow(ms) {
  const until = Math.min(Date.now() + ms, pollAnchor + POLL_EXTEND_CAP_MS);
  if (until > pollUntil) pollUntil = until;
  if (pollUntil > Date.now()) ensurePollLoop();
}

/* トリガー（操作）起点で窓を開く。延長上限の基準もここに移る。 */
function bumpPollWindow(ms) {
  pollAnchor = Date.now();
  extendPollWindow(ms);
}

function ensurePollLoop() {
  if (pollLoopRunning) return;
  pollLoopRunning = true;
  const tick = async () => {
    if (Date.now() >= pollUntil) { pollLoopRunning = false; return; }
    await pollOnce();
    setTimeout(tick, POLL_MS);
  };
  // 窓が開く直前に必ず取得済み（操作の同期確認 / refreshOnce）なので 1 周期待ってから回す。
  setTimeout(tick, POLL_MS);
}

/* 表示トリガー: 1 回だけ取得。動作中が見えたときだけ pollOnce 側で窓が開く。 */
async function refreshOnce() {
  pollAnchor = Date.now();
  await pollOnce();
}
```

- [ ] **Step 4: `pollOnce` 末尾に動作中の延長判定を足す**

`index.html:858`（`pollOnce` 内の `updateGroupSummaries();`）の直後に追加する:

```js
  // 動作中のシャッターが見えている間は窓を延長して完了まで追う（上限 POLL_EXTEND_CAP_MS）。
  for (const c of cards.values()) {
    if (c.kind !== "light" && MOVING_STATES.has(c.state)) {
      extendPollWindow(POLL_EXTEND_MS);
      break;
    }
  }
```

置き換え後の `pollOnce` 全体:

```js
async function pollOnce() {
  if (busyCount > 0) return;
  for (const [name, c] of cards) {
    if (busyCount > 0) return;
    // light は定期ポーリングしない（mat 直叩きは遅く、exec 直列を詰まらせる）。
    // 表示時 1 回 + 操作後の再取得のみ（fetchLightStatesOnce / scheduleLightCatchup）。
    if (c.kind === "light") continue;
    try {
      const view = await api("GET", `/api/devices/${encodeURIComponent(name)}/state`);
      renderState(name, view);
    } catch (e) {
      if (c) { c.msgEl.textContent = "接続なし"; c.statusEl.classList.add("error"); }
    }
  }
  updateGroupSummaries();
  // 動作中のシャッターが見えている間は窓を延長して完了まで追う（上限 POLL_EXTEND_CAP_MS）。
  for (const c of cards.values()) {
    if (c.kind !== "light" && MOVING_STATES.has(c.state)) {
      extendPollWindow(POLL_EXTEND_MS);
      break;
    }
  }
}
```

（state 取得エラー時は `c.state` が変わらないため、エラーで窓が延びることはない。
直前の取得で `opening` のまま残った場合は延長されるが、これは仕様どおり上限
`POLL_EXTEND_CAP_MS` で止まる。）

- [ ] **Step 5: 表示トリガーを 1 回取得に差し替える**

`index.html:1192` の

```js
  startPolling();
```

を

```js
  refreshOnce();
```

に、`index.html:1196` の

```js
document.addEventListener("visibilitychange", () => { if (!document.hidden) pollOnce(); });
```

を

```js
document.addEventListener("visibilitychange", () => { if (!document.hidden) refreshOnce(); });
```

に置き換える。

- [ ] **Step 6: ビルドと静的検査**

Run: `cargo build && cargo test && cargo clippy -- -D warnings`
Expected: すべて成功（JS は `include_str!` で焼き込まれるだけなのでビルド影響なし。Rust テストは無変更のまま PASS）。

さらに `grep -n "startPolling\|polling" index.html` を実行し、旧 `startPolling` / `polling` フラグへの参照が残っていないこと（`pollLoopRunning` 等の新名のみヒット）を確認する。

- [ ] **Step 7: Commit**

```bash
git add index.html
git commit -m "feat: 常時ポーリングをアクティブ窓（期限つきループ）に置き換え"
```

---

### Task 2: 操作完了後に 2 分窓を開く

**Files:**
- Modify: `index.html:813-817`（`deviceAct` の finally）
- Modify: `index.html:836-840`（`groupAct` の finally）

**Interfaces:**
- Consumes: Task 1 の `bumpPollWindow(ms)` と `POLL_WINDOW_MS`。
- Produces: なし（末端の呼び出し）。

- [ ] **Step 1: `deviceAct` の finally に窓オープンを足す**

`index.html` の `deviceAct` 内

```js
  } finally {
    setDeviceBusy(name, false);
    busyCount--;
    updateGroupSummaries();
  }
```

を以下に置き換える:

```js
  } finally {
    setDeviceBusy(name, false);
    busyCount--;
    updateGroupSummaries();
    // set 後の実状態変化（動作完了）を追うための窓。light はここに来ない（早期 return 済み）。
    bumpPollWindow(POLL_WINDOW_MS);
  }
```

（`deviceAct` は light の場合、冒頭で `lightAct` に委譲して return するため、この
finally はシャッター系のみ通る。）

- [ ] **Step 2: `groupAct` の finally に窓オープンを足す**

`index.html` の `groupAct` 内

```js
  } finally {
    setAllButtonsDisabled(false);
    busyCount--;
    updateGroupSummaries();
  }
```

を以下に置き換える:

```js
  } finally {
    setAllButtonsDisabled(false);
    busyCount--;
    updateGroupSummaries();
    bumpPollWindow(POLL_WINDOW_MS); // set 後の実状態変化（動作完了）を追うための窓。
  }
```

- [ ] **Step 3: ビルド確認**

Run: `cargo build && cargo clippy -- -D warnings`
Expected: 成功。

- [ ] **Step 4: Commit**

```bash
git add index.html
git commit -m "feat: シャッター操作後 2 分間だけ state ポーリングする窓を開く"
```

---

### Task 3: CLAUDE.md 原則 6 の更新と動作確認

**Files:**
- Modify: `CLAUDE.md`（設計原則 6「状態は pull」）

**Interfaces:**
- Consumes: Task 1・2 の完成した挙動。
- Produces: なし。

- [ ] **Step 1: 原則 6 の記述を実態に合わせる**

`CLAUDE.md` の

```markdown
6. **状態は pull。** 下層は one-shot で状態を持たないので push する主体がいない。UI は 3〜5 秒ポーリングで state を取得する。INF 通知を拾うための常駐化は下層の思想に反するのでやらない。
```

を以下に置き換える:

```markdown
6. **状態は pull、ただしアクティブ窓の間だけ。** 下層は one-shot で状態を持たないので push する主体がいない。UI は表示時に 1 回取得し、操作後 2 分間（`opening`/`closing` が見えている間は延長、直近トリガーから上限 10 分）だけ 3〜5 秒ポーリングする。窓の外ではポーリングしない（静止したシャッターの状態は勝手に変わらない）。INF 通知を拾うための常駐化は下層の思想に反するのでやらない。
```

- [ ] **Step 2: 実機動作確認**

Run: `RUST_LOG=debug cargo run`（実 config のある環境。手元に無ければデプロイ後に確認）

ブラウザでページを開き、サーバログで以下を確認する:

1. 表示直後に各シャッターの state 取得（enl exec）が 1 巡だけ走り、その後 exec ログが止まる。
2. シャッターを操作すると、完了後およそ 2 分間 4 秒間隔で state 取得が続き、その後止まる。
3. 操作直後の「開いています…」表示が、動作完了後に「開」へ自動で変わる（窓内のポーリングが拾う）。
4. タブを裏に回して戻すと 1 巡だけ取得され、その後止まる。

Expected: 上記 4 点すべて成立。静止時に enl exec が発生し続けないこと。

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: 原則 6 をアクティブ窓ポーリングに更新"
```
