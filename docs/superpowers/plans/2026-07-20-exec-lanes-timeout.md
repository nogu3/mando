# exec レーン直列化 + デバイス exec timeout 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** exec の直列化をグローバル 1 本から config 宣言のレーン単位に分割し、デバイス exec に timeout（デフォルト 15 秒・config 可変）を付ける。

**Architecture:** `Executor` をレーン名 → Semaphore(1) の遅延生成マップに変更。レーンは `Device.lane`（省略時デバイス名）で決まり、バックエンド知識は config に閉じたまま。デバイス exec は graph query と同じ形の timeout ラッパ `run_bounded` で包み、超過は既存の `ExecOutcome::Timeout` に写す。

**Tech Stack:** Rust / tokio / axum / serde+toml。詳細設計: `docs/superpowers/specs/2026-07-20-exec-lanes-timeout-design.md`

## Global Constraints

- **git commit / git add を実行しない。** ユーザーが並列セッションで同リポジトリを触っており、コミットは全実装完了後にユーザー確認のうえ別途行う。各タスクは「テスト green」で完了とする。
- 各タスク完了時に `cargo test` 全 green と `cargo clippy -- -D warnings` クリーンを確認する。
- 作業ブランチ: `living-lights-group`（現行ブランチのまま。ブランチ操作もしない）。
- timeout デフォルトは **15000ms**、config キーは `[exec] timeout_ms`（スペック記載値）。

---

### Task 1: Executor のレーン化（src/exec.rs）

**Files:**
- Modify: `src/exec.rs`

**Interfaces:**
- Produces: `Executor::run(&self, lane: &str, cmd: &[String]) -> ExecResult`（従来の `run(&self, cmd)` から変更。同じ `lane` 文字列は直列、異なる `lane` は並列）。`Executor::new()` は変更なし。
- 注意: このタスク完了時点では `src/main.rs` がコンパイルエラーになる（呼び出し側は Task 3 で直す）。テストは `cargo test --lib` 相当ではなく `cargo test exec::` も通らないため、**このタスクに限り main.rs の全 `executor.run(` / `graph_executor.run(` 呼び出しに仮レーン引数を足してビルドを通す**（`run(&device.get_state)` → `run("tmp", &device.get_state)` 等。Task 3 で正式値に置換する）。

- [ ] **Step 1: 失敗するテストを書く**

`src/exec.rs` の `mod tests` に追加。既存の `serializes_concurrent_calls` は lane 引数を足して「同一レーン」テストとして維持する（`ex.run(...)` → `ex.run("a", ...)`）。既存の他テストも `run("a", ...)` に更新する。新規テスト:

```rust
    #[tokio::test]
    async fn different_lanes_run_in_parallel() {
        use std::sync::Arc;
        use std::time::Instant;
        // 0.3 秒 sleep を別レーンで同時に走らせ、直列（0.6 秒超）に
        // ならないことを経過時間で確認する。
        let ex = Arc::new(Executor::new());
        let start = Instant::now();
        let a = {
            let ex = ex.clone();
            tokio::spawn(
                async move { ex.run("lane_a", &["sleep".into(), "0.3".into()]).await },
            )
        };
        let b = {
            let ex = ex.clone();
            tokio::spawn(
                async move { ex.run("lane_b", &["sleep".into(), "0.3".into()]).await },
            )
        };
        a.await.unwrap();
        b.await.unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_millis(550),
            "different lanes should run in parallel: {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn same_lane_is_serialized_by_elapsed_time() {
        use std::sync::Arc;
        use std::time::Instant;
        let ex = Arc::new(Executor::new());
        let start = Instant::now();
        let a = {
            let ex = ex.clone();
            tokio::spawn(async move { ex.run("lane", &["sleep".into(), "0.3".into()]).await })
        };
        let b = {
            let ex = ex.clone();
            tokio::spawn(async move { ex.run("lane", &["sleep".into(), "0.3".into()]).await })
        };
        a.await.unwrap();
        b.await.unwrap();
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(600),
            "same lane must serialize: {:?}",
            start.elapsed()
        );
    }
```

- [ ] **Step 2: テストが失敗する（コンパイルエラーになる）ことを確認**

Run: `cargo test --bin mando exec::`
Expected: コンパイルエラー（`run` は引数 1 個のため）。

- [ ] **Step 3: 実装**

`src/exec.rs` の `Executor` を置き換える:

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// exec 直列化器。レーン（文字列キー）ごとに Semaphore(1) を持ち、
/// 同一レーンの subprocess を直列化する。異なるレーンは並列に走る。
///
/// レーンの決め方は呼び出し側（config）の責務 — echonet 系（enl / casa 経由）は
/// 3610 を専有 bind するため同一レーンに集め、それ以外はデバイス単位でよい。
pub struct Executor {
    lanes: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            lanes: Mutex::new(HashMap::new()),
        }
    }

    /// レーンの Semaphore を取得（無ければ作る）。
    fn lane(&self, name: &str) -> Arc<Semaphore> {
        let mut lanes = self.lanes.lock().expect("lanes poisoned");
        lanes
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    /// コマンド配列を exec する。同一 lane 内で直列化される。
    ///
    /// `cmd[0]` を実行ファイル、残りを引数として扱う。空配列は呼び出し側で
    /// 弾く前提（config validate 済み）。
    pub async fn run(&self, lane: &str, cmd: &[String]) -> ExecResult {
        let sem = self.lane(lane);
        let _permit = sem.acquire_owned().await.expect("semaphore closed");

        let (program, args) = cmd.split_first().expect("empty command");
        tracing::debug!(lane, program, ?args, "exec");

        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // タイムアウト等で future が drop されたとき子プロセスを残さない。
            .kill_on_drop(true)
            .output()
            .await;

        match output {
            Ok(out) => {
                let outcome = ExecOutcome::from_code(out.status.code());
                ExecResult {
                    outcome,
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "spawn failed");
                ExecResult {
                    outcome: ExecOutcome::SpawnFailed,
                    stdout: String::new(),
                    stderr: e.to_string(),
                }
            }
        }
    }
}
```

`use tokio::sync::Semaphore;` は既存。spawn 以降のボディは現行の `run` と同一（変更点は冒頭の permit 取得と `tracing::debug!` への lane 追加のみ）。
main.rs のコンパイルを通すため、全 `.run(` 呼び出しに仮レーン `"tmp"` を足す
（`app.executor.run("tmp", &device.get_state)` / `executor.run("tmp", cmd)` 等、
このタスクではビルドを通すことだけが目的。正式値は Task 3）。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test`
Expected: 全 green（既存 exec テスト 5 本 + 新規 2 本を含む）。

Run: `cargo clippy -- -D warnings`
Expected: warning なし。

---

### Task 2: config に lane と [exec] timeout_ms を追加（src/config.rs）

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Consumes: なし（独立）。
- Produces:
  - `Device.lane: Option<String>`（serde default）
  - `Device::exec_lane(&self) -> &str`（lane 未指定ならデバイス名）
  - `Config.exec: ExecSettings`（serde default）
  - `pub struct ExecSettings { pub timeout_ms: u64 }`（Default = 15000）

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の既存 `#[cfg(test)] mod tests` に追加（既存テストの toml 文字列パターンに合わせる。`Config` は `Deserialize` なので `toml::from_str::<Config>` で直接パースできる）:

```rust
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
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando config::`
Expected: コンパイルエラー（`lane` / `exec_lane` / `exec` フィールド未定義）。

- [ ] **Step 3: 実装**

`Config` に追加（`health` フィールドの後）:

```rust
    /// デバイス exec の全体設定（任意）。
    #[serde(default)]
    pub exec: ExecSettings,
```

`Config` 定義の近く（`default_bind` の前あたり）に追加:

```rust
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
```

`Device` にフィールド追加（`members` の後）:

```rust
    /// exec 直列化レーン（任意）。同じ lane のデバイスと直列化される。
    /// 未指定ならデバイス名 = デバイス単位の直列化のみ（他デバイスとは並列）。
    /// echonet 系（enl / casa 経由）は 3610 を専有 bind するため、
    /// 同一の lane（例 "echonet"）を明示すること。
    #[serde(default)]
    pub lane: Option<String>,
```

`impl Device` にヘルパ追加:

```rust
    /// exec 直列化レーン名。未指定ならデバイス名。
    pub fn exec_lane(&self) -> &str {
        self.lane.as_deref().unwrap_or(&self.name)
    }
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test`
Expected: 全 green。

Run: `cargo clippy -- -D warnings`
Expected: warning なし。

---

### Task 3: main.rs — run_bounded 共通化とレーン・timeout の結線

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: Task 1 の `Executor::run(lane, cmd)`、Task 2 の `Device::exec_lane()` / `Config.exec.timeout_ms`。
- Produces: `run_bounded(executor: &Executor, lane: &str, cmd: &[String], timeout: Duration) -> ExecResult`（旧 `run_graph_cmd` を改名・汎用化。デバイスとグラフ両方が使う）。

- [ ] **Step 1: 失敗するテストを書く**

`src/main.rs` の既存 `#[cfg(test)] mod tests` に追加。既存のテスト用 `App` 構築パターン（`Executor::new()` を直接持たせている箇所）に合わせ、config は `toml::from_str` で作る:

```rust
    #[tokio::test]
    async fn device_exec_times_out_and_maps_to_timeout_outcome() {
        use std::time::Instant;
        let config: Config = toml::from_str(
            r#"
            [exec]
            timeout_ms = 200

            [[device]]
            name = "slow"
            kind = "light"
            get_state = ["sleep", "60"]
            on = ["true"]
            off = ["true"]
            "#,
        )
        .unwrap();
        let app = App {
            config,
            executor: Executor::new(),
            graph_executor: Executor::new(),
        };
        let device = app.config.find("slow").unwrap();
        let start = Instant::now();
        let view = fetch_state(&app, device).await;
        assert_eq!(view.exec, ExecOutcome::Timeout);
        assert_eq!(view.state, normalize::State::Unknown);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "timeout must bound the exec: {:?}",
            start.elapsed()
        );
    }
```

既存テスト `graph_query_timeout_maps_to_timeout_outcome` は `run_bounded` 改名に合わせて更新する（アサーション内容は維持）。
（`StateView.exec` / `normalize::State` の比較に `PartialEq` が足りなければ derive を足す。既存 `DeviceState` は `PartialEq` 実装済みのはず — 無ければ追加してよい。）

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando device_exec_times_out`
Expected: コンパイルエラーまたは FAIL（timeout 結線がまだ無く、`sleep 60` を待ち続けるなら test が長時間走る前にビルド段階で落ちる。`run_bounded` 未定義のため通常はコンパイルエラー）。

- [ ] **Step 3: 実装**

1. `run_graph_cmd` を `run_bounded` に改名し、lane 引数を追加。stderr メッセージを汎用化:

```rust
/// exec を timeout 付きで走らせる。超過は Timeout として返す
/// （future の drop で permit は解放され、子プロセスは kill_on_drop で回収される）。
async fn run_bounded(
    executor: &Executor,
    lane: &str,
    cmd: &[String],
    timeout: std::time::Duration,
) -> exec::ExecResult {
    match tokio::time::timeout(timeout, executor.run(lane, cmd)).await {
        Ok(r) => r,
        Err(_) => exec::ExecResult {
            outcome: ExecOutcome::Timeout,
            stdout: String::new(),
            stderr: "exec timeout".into(),
        },
    }
}
```

2. `App` にヘルパ追加:

```rust
impl App {
    /// デバイス exec の上限（config の [exec] timeout_ms）。
    fn exec_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.exec.timeout_ms)
    }
}
```

3. デバイス系 3 箇所を結線（Task 1 の仮レーン `"tmp"` をすべて置換する）:

- `fetch_state`:
  `let result = run_bounded(&app.executor, device.exec_lane(), &device.get_state, app.exec_timeout()).await;`
- `run_action`:
  `let result = run_bounded(&app.executor, device.exec_lane(), cmd, app.exec_timeout()).await;`
- `run_light_action`:
  `let result = run_bounded(&app.executor, device.exec_lane(), cmd, app.exec_timeout()).await;`

4. グラフ / health の 2 箇所は固定レーン `"graph"`（従来どおりグラフ・health 相互で直列）:

- `run_bounded(&app.graph_executor, "graph", &cmd, GRAPH_QUERY_TIMEOUT).await`
- `run_bounded(&app.graph_executor, "graph", &health.command, GRAPH_QUERY_TIMEOUT).await`

5. リポジトリ全体で `"tmp"` レーンが残っていないことを確認:

Run: `grep -n '"tmp"' src/`
Expected: ヒットなし。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test`
Expected: 全 green（新規 timeout テスト含む）。

Run: `cargo clippy -- -D warnings`
Expected: warning なし。

---

### Task 4: ドキュメント更新（CLAUDE.md / config.example.toml）

**Files:**
- Modify: `CLAUDE.md`（設計原則 5）
- Modify: `config.example.toml`

**Interfaces:**
- Consumes: Task 2 の config キー名（`lane` / `[exec] timeout_ms`）。

- [ ] **Step 1: CLAUDE.md 設計原則 5 を書き換える**

現行:

> 5. **subprocess は直列化する。** `enl` は `0.0.0.0:3610` を専有 bind する。`casa` 経由でも `casa` が `enl` を呼ぶので透過的に同じ衝突が起きる。よって **exec 全体を `Semaphore(1)` で囲い、並行に走らせない**（axum は非同期だが、ここだけは意図的に直列）。

置換後:

> 5. **subprocess はレーン単位で直列化し、timeout で有界にする。** `enl` は `0.0.0.0:3610` を専有 bind する（`casa` 経由でも `casa` が `enl` を呼ぶので透過的に同じ衝突が起きる）。よって echonet 系デバイスは config の `lane = "echonet"` で**同一レーンに集めて直列化**する。レーン未指定のデバイスはデバイス名がレーンになり、自分自身の操作・state 読みだけが直列（他デバイスとは並列）。mat は matd が並行を捌くのでレーン不要。全デバイス exec は `[exec] timeout_ms`（既定 15000）で打ち切り、ハングが他レーンや UI を巻き込まないようにする（超過は「応答なし、もう一度」）。バックエンドがどのレーンに属すべきかの知識は config に置き、本体は知らない（原則 2 と同型）。

- [ ] **Step 2: config.example.toml に lane と [exec] を追記**

ファイル冒頭付近（bind の説明の近く）に追加:

```toml
# デバイス exec の上限ミリ秒（省略時 15000）。超過は「応答なし」として UI に出る。
[exec]
timeout_ms = 15000
```

echonet 系デバイス例（enl / casa を使う [[device]]）に `lane` 行とコメントを追加:

```toml
[[device]]
name      = "shutter"
# enl / casa 経由のデバイスは 3610 を専有 bind するため、同一レーンで直列化する。
# mat 等それ以外のバックエンドでは lane 不要（省略時はデバイス単位の直列のみ）。
lane      = "echonet"
get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
...
```

（既存の例にある enl/casa 系 device すべてに `lane = "echonet"` を付ける。mat 系の例には付けない。）

- [ ] **Step 3: 検証**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全 green / warning なし（ドキュメントのみの変更で回帰がないことの確認）。

Run: `grep -n "Semaphore(1) で囲い" CLAUDE.md`
Expected: ヒットなし（旧記述が残っていない）。

---

## 完了後（計画外・ユーザー確認後に別途）

1. ユーザーレビュー → コミット（並列セッションの都合でユーザー確認後）
2. cross build → despliegue skill で jarvis 配布
3. jarvis-iac `roles/mando/files/config.toml` に echonet 系 10 台（shutter1〜5 / entrance_indirect_light / hallway_floor_light / kitchen_light / washstand_light / wic_downlight）へ `lane = "echonet"` を追記し、`[exec]` を追加 → Ansible 適用
4. mando UI で mat 照明と echonet 照明の同時操作を確認
