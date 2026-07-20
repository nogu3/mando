# state 読み short-TTL + single-flight キャッシュ 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `GET /api/devices/{name}/state` に short-TTL + single-flight のキャッシュ層を挟み、アクティブ窓ポーリングで近接した複数クライアントの state 読みを 1 回の exec に束ねる。

**Architecture:** デバイス名をキーにした per-key `tokio::sync::Mutex` で「TTL 内なら再 exec しない」と「同時読みは進行中の 1 exec を共有」を 1 ロックで両立する汎用キャッシュ `Cache<T>` を新設（`src/cache.rs`）。`get_state` ハンドラはこれ経由に。set 経路は従来どおりキャッシュ非経由で state を再取得し、成功時のみ確定値でキャッシュを上書きする（原則7）。

**Tech Stack:** Rust, tokio, axum, serde/toml。既存 `Executor`（レーン直列化）とは独立。

## Global Constraints

- **成功読みだけキャッシュ。** `exec != ExecOutcome::Success` は保存しない（失敗をピン留めして再試行を殺さない）。
- **set はキャッシュ非経由。** `run_action` の post-set 再取得は生の `fetch_state`。その確定値（Success 時のみ）で `store` する。
- **既定 TTL 2000ms、`0` は TTL 無効（single-flight のみ）。** ポーリング間隔 3〜5 秒より十分短く。
- 対象は device state のみ。graph / health は対象外。
- `cargo test` と `cargo clippy -- -D warnings` が通ること。
- バックエンド固有知識を持ち込まない（TTL は config 値、本体は数値として扱うだけ）。

---

## File Structure

- `src/config.rs` — `CacheSettings { state_ttl_ms }` を追加、`Config.cache` フィールド。
- `src/cache.rs`（新規）— 汎用 `Cache<T>`：`get_or_fetch` / `store`。StateView に依存しない（`T` はジェネリック）ので単体テスト可能。
- `src/main.rs` — `mod cache;`、`App.state_cache: Cache<StateView>` を追加（4 つの構築箇所）、`StateView` に `Clone`、`cached_state` ヘルパ、`get_state` と `run_action` を接続。
- `config.example.toml` — `[cache]` の例。

---

## Task 1: config に `[cache] state_ttl_ms` を追加

**Files:**
- Modify: `src/config.rs`（`Config` 構造体 53-68 行、`ExecSettings` の直後 84 行あたりに追記）
- Test: `src/config.rs`（末尾 `#[cfg(test)]` に追加）

**Interfaces:**
- Produces: `CacheSettings { pub state_ttl_ms: u64 }`（`Deserialize + Clone + Debug + Default`、既定 2000）、`Config.cache: CacheSettings`。

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `#[cfg(test)] mod tests` 内に追加:

```rust
#[test]
fn cache_defaults_to_2000ms() {
    let cfg: Config = toml::from_str(r#"bind = "0.0.0.0:8080""#).unwrap();
    assert_eq!(cfg.cache.state_ttl_ms, 2000);
}

#[test]
fn cache_ttl_is_overridable() {
    let cfg: Config = toml::from_str("[cache]\nstate_ttl_ms = 500\n").unwrap();
    assert_eq!(cfg.cache.state_ttl_ms, 500);
}
```

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test --lib config::tests::cache_defaults_to_2000ms`
Expected: FAIL（`no field 'cache' on type Config` でコンパイルエラー）

- [ ] **Step 3: 実装**

`Config` 構造体（`pub exec: ExecSettings,` の直後）に追加:

```rust
    /// state 読みキャッシュの設定（任意）。
    #[serde(default)]
    pub cache: CacheSettings,
```

`ExecSettings` の `default_exec_timeout_ms` 定義（88 行）の直後に追加:

```rust
/// state 読みキャッシュの設定。
#[derive(Debug, Clone, Deserialize)]
pub struct CacheSettings {
    /// state 読みをキャッシュする TTL（ミリ秒）。0 は TTL 無効（single-flight のみ）。
    #[serde(default = "default_state_ttl_ms")]
    pub state_ttl_ms: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        CacheSettings {
            state_ttl_ms: default_state_ttl_ms(),
        }
    }
}

fn default_state_ttl_ms() -> u64 {
    2_000
}
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test --lib config::tests::cache_`
Expected: PASS（2 件）

- [ ] **Step 5: コミット**

```bash
git add src/config.rs
git commit -m "feat(config): [cache] state_ttl_ms を追加(既定 2000ms)"
```

---

## Task 2: 汎用キャッシュ `Cache<T>`（`src/cache.rs`）

**Files:**
- Create: `src/cache.rs`
- Modify: `src/main.rs`（`mod exec;` などの隣に `mod cache;` を追加）
- Test: `src/cache.rs`（同ファイル内 `#[cfg(test)]`）

**Interfaces:**
- Produces:
  - `Cache<T: Clone + Send + 'static>`（`Default` 実装あり）
  - `async fn get_or_fetch<F, Fut>(&self, key: &str, ttl: Duration, fetch: F) -> T where F: FnOnce() -> Fut, Fut: Future<Output = (T, bool)>` — 戻り値 `(value, cacheable)`。`cacheable` が true のときだけ保存する。
  - `async fn store(&self, key: &str, value: T)` — 確定値でキャッシュを上書き（set 経路用）。

**設計メモ（freshness 判定）:** リクエスト到着時刻 `arrival` を関数入口で採る。per-key ロック取得後、保持中の `Cached` について
`at.elapsed() < ttl`（TTL ヒット）**または** `at >= arrival`（自分が待つ間に他リクエストが計算した値＝single-flight 合流）なら再 exec せずそれを返す。`ttl == 0` でも後者により同時読みは合流する。

- [ ] **Step 1: 失敗するテストを書く**

`src/cache.rs` を新規作成:

```rust
//! （実装は Step 3 で入れる。まずテストだけ）
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn ttl_hit_skips_fetch() {
        let cache: Cache<u32> = Cache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_millis(500);

        let c = calls.clone();
        let a = cache
            .get_or_fetch("k", ttl, || async move {
                c.fetch_add(1, Ordering::SeqCst);
                (7, true)
            })
            .await;
        let c = calls.clone();
        let b = cache
            .get_or_fetch("k", ttl, || async move {
                c.fetch_add(1, Ordering::SeqCst);
                (99, true)
            })
            .await;

        assert_eq!(a, 7);
        assert_eq!(b, 7, "TTL 内は 1 回目の値を返す");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fetch は 1 回だけ");
    }

    #[tokio::test]
    async fn ttl_expiry_refetches() {
        let cache: Cache<u32> = Cache::default();
        let ttl = Duration::from_millis(30);
        let a = cache.get_or_fetch("k", ttl, || async { (1, true) }).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        let b = cache.get_or_fetch("k", ttl, || async { (2, true) }).await;
        assert_eq!(a, 1);
        assert_eq!(b, 2, "TTL 経過後は再 exec");
    }

    #[tokio::test]
    async fn non_cacheable_is_not_stored() {
        let cache: Cache<u32> = Cache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_millis(500);
        for _ in 0..2 {
            let c = calls.clone();
            cache
                .get_or_fetch("k", ttl, || async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    (0, false) // cacheable=false（失敗相当）
                })
                .await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "失敗はキャッシュされず毎回 fetch");
    }

    #[tokio::test]
    async fn single_flight_coalesces_with_zero_ttl() {
        // ttl=0 でも、同時に走る同一キー読みは 1 fetch に合流する。
        let cache: Arc<Cache<u32>> = Arc::new(Cache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::ZERO;

        let mut handles = vec![];
        for _ in 0..5 {
            let cache = cache.clone();
            let c = calls.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_fetch("k", ttl, || async move {
                        // 1 発目が握っている間に他が到着するよう、少し待つ。
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        c.fetch_add(1, Ordering::SeqCst);
                        (42, true)
                    })
                    .await
            }));
        }
        for h in handles {
            assert_eq!(h.await.unwrap(), 42);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "同時読みは 1 fetch に合流");
    }

    #[tokio::test]
    async fn store_overwrites_for_subsequent_reads() {
        let cache: Cache<u32> = Cache::default();
        let ttl = Duration::from_millis(500);
        cache.store("k", 5).await;
        let v = cache
            .get_or_fetch("k", ttl, || async { (999, true) })
            .await;
        assert_eq!(v, 5, "store 済みの確定値を TTL 内は返す");
    }

    #[tokio::test]
    async fn distinct_keys_are_independent() {
        let cache: Cache<u32> = Cache::default();
        let ttl = Duration::from_millis(500);
        let a = cache.get_or_fetch("a", ttl, || async { (1, true) }).await;
        let b = cache.get_or_fetch("b", ttl, || async { (2, true) }).await;
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }
}
```

`src/main.rs` の `mod exec;`（先頭付近のモジュール宣言）の隣に追加:

```rust
mod cache;
```

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test --lib cache::tests`
Expected: FAIL（`cannot find type 'Cache'` でコンパイルエラー）

- [ ] **Step 3: 実装**

`src/cache.rs` の先頭（`#[cfg(test)]` の前）に実装を追加:

```rust
//! state 読みの short-TTL + single-flight キャッシュ。
//!
//! デバイス名をキーに per-key の `tokio::sync::Mutex` を持ち、同一キーの読みを
//! 直列化する。TTL 内なら再 exec を省き、待機中に他リクエストが計算した値は
//! 共有する（single-flight）。TTL とバックエンド知識は呼び出し側の責務で、
//! この層は「文字列キー → 値 T」を短時間共有するだけ。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Cached<T> {
    at: Instant,
    value: T,
}

/// 文字列キーごとに値 T を短時間共有する汎用キャッシュ。
pub struct Cache<T> {
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Cached<T>>>>>>,
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Cache {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: Clone + Send + 'static> Cache<T> {
    /// key の per-key ロックを取得（無ければ作る）。保持は一瞬。
    fn slot(&self, key: &str) -> Arc<tokio::sync::Mutex<Option<Cached<T>>>> {
        let mut slots = self.slots.lock().expect("cache slots poisoned");
        slots
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
            .clone()
    }

    /// TTL 内 or 待機中に計算された値があればそれを返し、無ければ `fetch` を走らせる。
    /// `fetch` は `(値, キャッシュ可否)` を返す。キャッシュ可否が false の結果は保存しない。
    pub async fn get_or_fetch<F, Fut>(&self, key: &str, ttl: Duration, fetch: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = (T, bool)>,
    {
        let arrival = Instant::now();
        let slot = self.slot(key);
        let mut guard = slot.lock().await;

        if let Some(c) = guard.as_ref() {
            // TTL ヒット、または自分が待つ間に他リクエストが入れた値（single-flight 合流）。
            if c.at.elapsed() < ttl || c.at >= arrival {
                return c.value.clone();
            }
        }

        let (value, cacheable) = fetch().await;
        if cacheable {
            *guard = Some(Cached {
                at: Instant::now(),
                value: value.clone(),
            });
        }
        value
    }

    /// 確定値でキャッシュを上書きする（set 後の再取得結果用）。
    pub async fn store(&self, key: &str, value: T) {
        let slot = self.slot(key);
        let mut guard = slot.lock().await;
        *guard = Some(Cached {
            at: Instant::now(),
            value,
        });
    }
}
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test --lib cache::tests`
Expected: PASS（6 件）

- [ ] **Step 5: clippy 確認**

Run: `cargo clippy --lib -- -D warnings`
Expected: 警告なし

- [ ] **Step 6: コミット**

```bash
git add src/cache.rs src/main.rs
git commit -m "feat(cache): short-TTL + single-flight の汎用 Cache<T>"
```

---

## Task 3: `App` に接続（get_state はキャッシュ経由、set は store）

**Files:**
- Modify: `src/main.rs`（`App` 構造体 29-36 行、`App` impl 38-43 行、`App` 構築 4 箇所、`StateView` 186-194 行、`get_state` 241-246 行、`run_action` 258-274 行）
- Test: `src/main.rs`（`#[cfg(test)] mod tests` に追加）

**Interfaces:**
- Consumes: `Cache<T>`（Task 2）、`CacheSettings`（Task 1）。
- Produces: `App.state_cache: Cache<StateView>`、`App::state_ttl() -> Duration`、`async fn cached_state(app: &App, device: &Device) -> StateView`。

- [ ] **Step 1: 失敗するテストを書く**

`src/main.rs` の `#[cfg(test)] mod tests` に追加。exec 回数は get_state テンプレが temp ファイルに追記した行数で数える（exec.rs と同手法）:

```rust
/// get_state が exec のたびに temp ファイルへ 1 行追記する shutter を持つ App。
/// `ttl_ms` でキャッシュ TTL を差し替える。
fn counting_app(counter_path: &str, ttl_ms: u64) -> Shared {
    let cfg: Config = toml::from_str(&format!(
        r##"
        [cache]
        state_ttl_ms = {ttl_ms}
        [[device]]
        name = "shutter"
        get_state = ["sh", "-c", "printf x >> {counter_path}; printf '{{\"properties\":[{{\"name\":\"open_close_state\",\"value\":\"open\"}}]}}'"]
        open  = ["sh", "-c", "printf '{{}}'"]
        close = ["sh", "-c", "printf '{{}}'"]
        "##
    ))
    .unwrap();
    Arc::new(App {
        config: cfg,
        executor: Executor::new(),
        graph_executor: Executor::new(),
        state_cache: cache::Cache::default(),
    })
}

fn exec_count(counter_path: &str) -> usize {
    std::fs::read_to_string(counter_path)
        .map(|s| s.len())
        .unwrap_or(0)
}

#[tokio::test]
async fn cached_state_hits_within_ttl() {
    let path = std::env::temp_dir().join(format!("mando_cache_hit_{}.txt", std::process::id()));
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "").unwrap();

    let app = counting_app(&p, 2000);
    let device = app.config.find("shutter").unwrap();

    let a = cached_state(&app, device).await;
    let b = cached_state(&app, device).await;

    std::fs::remove_file(&path).ok();
    assert_eq!(a.state, normalize::State::Open);
    assert_eq!(b.state, normalize::State::Open);
    assert_eq!(exec_count(&p), 1, "TTL 内の 2 回目は exec しない");
}

#[tokio::test]
async fn cached_state_refetches_after_ttl() {
    let path = std::env::temp_dir().join(format!("mando_cache_exp_{}.txt", std::process::id()));
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "").unwrap();

    let app = counting_app(&p, 30);
    let device = app.config.find("shutter").unwrap();

    cached_state(&app, device).await;
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    cached_state(&app, device).await;

    std::fs::remove_file(&path).ok();
    assert_eq!(exec_count(&p), 2, "TTL 経過後は再 exec");
}
```

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test --lib tests::cached_state_hits_within_ttl`
Expected: FAIL（`missing field 'state_cache'` / `cannot find function 'cached_state'` でコンパイルエラー）

- [ ] **Step 3: `App` にフィールドと TTL ヘルパを追加**

`App` 構造体（`graph_executor: Executor,` の直後）に:

```rust
    /// state 読みの short-TTL + single-flight キャッシュ（原則6/7）。
    state_cache: cache::Cache<StateView>,
```

`App` impl の `exec_timeout` の直後に:

```rust
    /// state 読みキャッシュの TTL（config の [cache] state_ttl_ms）。
    fn state_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.cache.state_ttl_ms)
    }
```

- [ ] **Step 4: 4 つの `App` 構築箇所に `state_cache` を追加**

以下の各 `Arc::new(App { ... })` / `App { ... }` に `state_cache: cache::Cache::default(),` を追加する:
- `main()` 内（`graph_executor: Executor::new(),` の行の後）
- `test_app()` 内
- `call_on()` 内
- timeout テスト（`fetch_state` を直接呼ぶテスト）内の `App { ... }`

例（`main()`）:

```rust
    let app = Arc::new(App {
        config,
        executor: Executor::new(),
        graph_executor: Executor::new(),
        state_cache: cache::Cache::default(),
    });
```

- [ ] **Step 5: `StateView` を `Clone` にする**

`StateView` の derive を変更:

```rust
#[derive(Serialize, Clone)]
struct StateView {
```

- [ ] **Step 6: `cached_state` ヘルパを追加し、`get_state` を接続**

`fetch_state` の直後に追加:

```rust
/// get_state をキャッシュ経由で実行する（GET ハンドラ用）。
/// 成功読みだけ TTL キャッシュし、同時読みは 1 exec に合流する（原則6/7）。
/// set 経路はこれを通さず、生の fetch_state + store を使う。
async fn cached_state(app: &App, device: &Device) -> StateView {
    let ttl = app.state_ttl();
    app.state_cache
        .get_or_fetch(&device.name, ttl, || async {
            let view = fetch_state(app, device).await;
            let cacheable = view.exec == ExecOutcome::Success;
            (view, cacheable)
        })
        .await
}
```

`get_state` ハンドラ内の `fetch_state(&app, device)` を `cached_state(&app, device)` に差し替え:

```rust
async fn get_state(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.config.find(&name) {
        Some(device) => Json(cached_state(&app, device).await).into_response(),
        None => not_found(&name),
    }
}
```

- [ ] **Step 7: `run_action` の post-set 確定値で `store`**

`run_action` の `let state = fetch_state(app, device).await;` の直後に、成功時のみ store を追加:

```rust
    // 設計原則 7: set 後は必ず state を取り直し、実際の開閉を確認してから返す。
    let state = fetch_state(app, device).await;
    // 確定値でキャッシュを更新（成功時のみ）。直後のポーリングが古い値を見ない。
    if state.exec == ExecOutcome::Success {
        app.state_cache
            .store(&device.name, state.clone())
            .await;
    }
    ActionView {
        action: result.outcome,
        state,
    }
```

- [ ] **Step 8: テスト全通過を確認**

Run: `cargo test --lib`
Expected: PASS（既存＋新規すべて）

- [ ] **Step 9: clippy 確認**

Run: `cargo clippy -- -D warnings`
Expected: 警告なし

- [ ] **Step 10: コミット**

```bash
git add src/main.rs
git commit -m "feat(api): state 読みをキャッシュ経由に、set 後は確定値を store"
```

---

## Task 4: `config.example.toml` に `[cache]` の例を追記

**Files:**
- Modify: `config.example.toml`（`[exec]` セクション 16-17 行の直後）
- Test: `src/config.rs`（`#[cfg(test)] mod tests` に example パーステストを追加）

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `#[cfg(test)] mod tests` に、同梱 example が正しくパースでき `[cache]` の意図した値になることを確認するテストを追加:

```rust
#[test]
fn example_config_parses_with_cache() {
    let src = include_str!("../config.example.toml");
    let cfg: Config = toml::from_str(src).expect("config.example.toml must parse");
    assert_eq!(cfg.cache.state_ttl_ms, 2000);
}
```

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test --lib config::tests::example_config_parses_with_cache`
Expected: FAIL（example に `[cache]` が無く `state_ttl_ms` が既定 2000 に一致するはずだが、明示値がないため意図を固定できていない。まず追記前に走らせて現状を確認 → 追記後に PASS させる）

- [ ] **Step 3: `config.example.toml` に追記**

`config.example.toml` の `[exec]` ブロック（`timeout_ms = 15000` の行）の直後に空行を挟んで追加:

```toml
# state 読みのキャッシュ。アクティブ窓ポーリングで近接した複数クライアントの
# 読みを 1 回の機器読み取りに束ね、3610 の直列待ちと負荷を抑える。
# 成功読みだけを state_ttl_ms の間だけ共有する（set は非経由・確定値で上書き）。
[cache]
state_ttl_ms = 2000   # 既定 2000。0 = TTL 無効（同時読みの合流のみ）
```

- [ ] **Step 4: テスト全通過を確認**

Run: `cargo test --lib config::tests::example_config_parses_with_cache`
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add config.example.toml src/config.rs
git commit -m "docs(config): config.example に [cache] state_ttl_ms を追記"
```

---

## Self-Review

**Spec coverage:**
- 目的とスコープ（state のみ）→ Task 3（`get_state` のみ接続）。
- `Cache<T>` 機構（per-key ロック、TTL、single-flight）→ Task 2。
- 正直さの不変条件（成功だけキャッシュ / set 非経由 + store / 失敗非キャッシュ）→ Task 2（cacheable フラグ、non_cacheable テスト）＋ Task 3（`cached_state` の cacheable 判定、`run_action` の store）。
- config `[cache] state_ttl_ms`（既定 2000、0 で TTL 無効）→ Task 1 ＋ Task 2（ttl=0 の single-flight テスト）。
- テスト（single-flight / TTL hit-miss / 失敗非キャッシュ / store / デバイス独立）→ Task 2 の 6 テスト ＋ Task 3 の 2 統合テスト。
- config.example → Task 4。

**Placeholder scan:** プレースホルダなし。全ステップに実コードあり。

**Type consistency:** `Cache<T>` / `get_or_fetch(key, ttl, fetch) -> T`（fetch は `(T, bool)`）/ `store(key, value)` を Task 2 で定義、Task 3 で `Cache<StateView>`・`cached_state` として一致利用。`StateView: Clone`、`normalize::State::Open`、`ExecOutcome::Success` は既存定義と一致。`App.state_cache` は 4 構築箇所すべてで追加。
