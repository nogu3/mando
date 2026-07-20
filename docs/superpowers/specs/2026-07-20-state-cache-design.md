# state 読みの short-TTL + single-flight キャッシュ 設計

**日付:** 2026-07-20
**対象:** CLAUDE.md ロードマップ Phase 2 「ポーリング結果の短 TTL キャッシュ — 複数クライアントが 1 回の機器読み取りを共有し、3610 への負荷と直列待ちを抑える」

---

## 目的とスコープ

`GET /api/devices/{name}/state` **のみ**を対象にキャッシュ層を挟む。アクティブ窓の
3〜5 秒ポーリング（原則6）で、近接して来た複数クライアントの state 読みを 1 回の
exec に束ね、echonet レーン（3610 専有 bind）の直列待ちと負荷を抑える。

**対象外:** graph / health。これらは embalse HTTP 経由（`graph_executor`）で 3610 と
無関係、かつビュー表示時 1 回きりでポーリングされずホットでない。キャッシュ機構自体は
デバイス名以外のキーにも流用可能な形にしておくが、適用は state のみに限定して正直さの
影響面を最小化する（将来 graph/health を足すのは容易）。

---

## キャッシュ機構（`StateCache`）

デバイス名をキーに per-key ロックを持つ二段構成:

```
StateCache {
    entries: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Cached>>>>>,
}

Cached { at: std::time::Instant, view: StateView }
```

- 外側 `std::sync::Mutex<HashMap<..>>` は per-key ロック（`Arc<tokio::sync::Mutex<..>>`）を
  取得/生成するだけ。保持は一瞬（await をまたがない）。
- 内側 per-key `tokio::sync::Mutex<Option<Cached>>` が single-flight と TTL の実体。

### 読み出し（`get_or_fetch`）

`GET` ハンドラが呼ぶ。TTL と、ミス時に走らせる fetch クロージャを受け取る:

1. 外側ロックで per-key ロックの `Arc` を取得/生成し、per-key ロックを取る。
   **同一デバイスの同時読みはここで直列化される（= single-flight の実体）。**
2. `at.elapsed() < ttl` なら、保持している `StateView` を clone して返す（exec しない）。
3. TTL 切れ / 未キャッシュなら実 exec（`fetch_state`）を走らせる。
   - **成功時のみ** `Cached { at: Instant::now(), view }` を保存。
   - 失敗（`exec != Success` / `Unknown`）は保存せず、その結果をそのまま返す。
4. per-key ロックを解放。後続の待機者はロック取得時点で 2 の分岐に入り、
   直前に取得された最新値を共有する（result が全員で共有される）。

異なるデバイスは別の per-key ロックなので互いに並列。既存の echonet レーン直列化
（`Executor` の Semaphore）とは独立で、レーンはデバイス横断の 3610 衝突を、
per-key ロックは同一デバイスの読み重複を、それぞれ抑える。

### 上書き（`store`）

set 経路が「set 後の確定 state」でキャッシュを更新するための入口。`Success` のときだけ
`Cached` を上書きする。

### 前提となる型変更

- `StateView` を `Clone` にする（`DeviceState` / `ExecOutcome` は既に `Clone`、
  `raw: Option<Value>` も `Clone`）。

---

## 正直さの不変条件（原則7）

- **成功読みだけキャッシュする。** `exec != Success`（Timeout / Rejected / NetworkError /
  SpawnFailed）や JSON パース失敗（`Unknown`）は保存しない。エラーをピン留めして
  再試行を殺さない。次の読みは必ず実 exec で再挑戦する。
- **set 系はキャッシュを一切通さない。** `run_action` の「set 後 state 再取得」
  （原則7 の同期確認）は今まで通り生の `fetch_state`。さらにその確定結果で
  `cache.store` を呼び上書きする。これで自分の操作直後のポーリングが古い
  open/closed を見ることがない。
- **light も同じ規則で統一。** light の非同期追いつき読み（押下 ~2 秒後の 1 回）も
  この GET 経路を通るので、ベストエフォートのまま束ねられる。light の set 例外
  （`run_light_action` は state 同梱なし）はそのまま。
- **マスクし得るのは「mando 外の物理変化を最大 TTL 秒」だけ**で、次のポーリング
  （≤ 間隔 + TTL）で必ず追いつく。原則6 が安全と宣言している「静止シャッターは
  勝手に動かない」域に収まる。

---

## config

新しい小さなテーブルを 1 つ追加:

```toml
[cache]
state_ttl_ms = 2000   # 既定 2000。0 = TTL 無効（single-flight のみ）
```

- ポーリング間隔（3〜5 秒）より十分短い 2 秒を既定に。単一クライアントが自分の
  ポーリング周期をまたいで古い値を見ることはない（周期 > TTL）。
- `0` は single-flight のみのエスケープハッチ（古さゼロ運用も config だけで選べる）。
- バックエンド固有の知識は持ち込まない（原則2 と同型）。`[cache]` 未指定なら既定値。

---

## 影響範囲

- `src/main.rs`: `App` に `state_cache: StateCache` を追加。`get_state` ハンドラを
  `cache.get_or_fetch` 経由に、`run_action` の post-set 確定後に `cache.store` を追加。
- 新規 or `exec.rs` 近傍に `StateCache`（キャッシュ機構）。`StateView` を `Clone` 化。
- `src/config.rs`: `[cache] state_ttl_ms`（`Option`、既定 2000）。
- `config.example.toml`: `[cache]` セクションのコメント付き例。

---

## テスト

- **single-flight:** 同一デバイスへ同時 N 読み → 実 fetch は 1 回だけ（fetch 呼び出しを
  カウンタで検証）。
- **TTL ヒット/ミス:** TTL 内の 2 回目読みは exec せずキャッシュ値を返す → TTL 経過後は
  再 exec（時間経過で検証、exec.rs の既存テストと同手法）。
- **失敗は非キャッシュ:** 1 回目 Timeout → 2 回目は再 exec される。
- **store 経由:** set 後の確定値を `store` した後、以後の読みがそれを返す。
- **デバイス独立:** 異なるデバイスは並列（別 per-key ロック、経過時間で確認）。

既存の `fetch_state` は fake config（`sh -c` でダミー JSON を吐くテンプレ）でテスト可能
なので、それに乗せる。
