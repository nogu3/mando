# light 状態の push 化（mat listen → SSE）実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** light の状態を read（pull）ではなく `mat listen`（push）から得るようにし、CASE cold-start による「応答なし／極端に遅い」を原理的に消す。UI までは SSE で push する。

**Architecture:** `[push] listen` に長寿命サブプロセスを 1 本張り（`src/push.rs` の listener タスク）、stdout の 1 行 1 JSON を `PushStore`（node_id → 論理デバイスの突合 + `"cluster/attribute" → 値` の汎用マップ）へ反映し、`tokio::sync::broadcast` で `GET /api/events`（SSE）へ扇形配信する。鮮度は TTL で腐らせず、**listener が生きているか**だけで信頼を決める（primed = exec ゼロで即答 / unprimed・断 = read で確定 / read も失敗 = `stale: true`）。イベント JSON の解釈は `normalize.rs` に閉じ、`push.rs` は下層非依存な機械に保つ。

**Tech Stack:** Rust / axum 0.7（`response::sse`）/ tokio（`process` + `io-util` + `sync`）/ serde / toml、フロントは依存ライブラリなしの素の HTML + `EventSource`。

## Global Constraints

- 設計: `docs/superpowers/specs/2026-07-25-light-push-state-design.md`（前提文書 `2026-07-10-light-async-state-design.md`）
- issue 番号は未採番。開いた場合はコミットメッセージ末尾に `（#N）` を付ける（このリポジトリの既存慣習）
- **実 IP・実 node_id・実 alias をリポジトリに書かない。** テストとサンプルはダミー値を使う
- `cargo test` が通ること。**既存 183 件が 1 件も壊れないこと**（本計画は 33 件を追加する）
- `cargo clippy --all-targets -- -D warnings` が通ること。**`-D warnings` は `dead_code` も含むので、各コミットは「追加したものをそのコミット内で消費する」形でなければならない** — タスク境界はこの制約で決まっている（下記 File Structure 参照）
- `[push]` 未設定なら push 機能は完全に無効で、既存挙動と 1 バイトも変わらない
- **shutter / switch / group / graphs / health / mesh の挙動と応答の形は変えない。** アクティブ窓ポーリング（`2026-07-18-active-window-polling-design.md`）には一切触らない
- 成果物はバイナリ 1 個 + `config.toml`（`index.html` / `mesh.html` は焼き込み）
- backoff の既定値: `BACKOFF_MIN = 1s`、`BACKOFF_MAX = 30s`、`BACKOFF_RESET_AFTER = 60s`
- broadcast バッファ: `BROADCAST_CAPACITY = 64`
- クライアント定数: `LIGHT_CATCHUP_MS = 2000`（既存・SSE 断時の fallback）、`LIGHT_SETTLE_MS = 4000`（新規・push を待つ見張り。空振り時のみ読む。当初 2000 → jarvis 実測の push 到達 0.8〜3.1 秒を吸収できず read を撒いたため 4000 へ）

## 設計からの逸脱・補足（5 点）

実装前に mat リポジトリの実装と README を読んで確認した結果、設計書の記述を 1 点訂正し、
設計書が触れていない 4 点を決めた。

### 1. `[push] listen` に `--count` を必ず渡す（設計書の config 例の訂正）

設計書の例は `listen = ["mat", "listen", "--timeout-ms", "0"]` だが、`mat listen` の
`--count` は **既定 1**（`clap` の `range(1..)` で 0 = 無限は無い。`crates/mat/src/cli.rs`）で、
「この件数を受けたら exit 0」という意味である。よってこの例では**イベント 1 件で
プロセスが終了する**。毎イベントごとに listener が落ちて再ベースライン read が走り、
本設計が消したかった read がそのまま戻ってきてしまう。

正しい例は `--count` に実質無限（u32 上限）を渡す:

```toml
[push]
listen = ["mat", "listen", "--count", "4294967295", "--timeout-ms", "0"]
```

mando 本体はコマンド配列をそのまま exec するだけなので（原則 2）、コード側の対応は不要。
`config.example.toml` とテストにこの形を使い、なぜ `--count` が必要かをコメントで残す。

### 2. `StateView.exec` を `Option<ExecOutcome>` にする

push 由来の即答では exec が 1 回も走らない。既存の `exec: ExecOutcome` に
`Success` を入れると「走らなかった exec の成功」を騙ることになり、原則 7 に反する。
`Option` にして push 即答では省略する（`skip_serializing_if`）。shutter / switch は
常に `Some` なので**応答 JSON は変わらない**。クライアントは既に
`STATE_MSG[view.exec] || ""` と書いており、フィールド欠落でも "" に落ちる。

### 3. SSE 接続中の「反映中…」後始末タイマー（設計書に無い穴）

設計書は「SSE 接続中は `scheduleLightCatchup` を張らない」としか書いていない。
しかし `apply` は**導出 state が変わったときだけ** broadcast する（変わらない更新で
全クライアントを起こさない）ので、**既に点いているライトの「つける」を押すと
イベントが来ず、タイルが「反映中…」で固まる**。

そこで SSE 接続中は `scheduleLightSettle(name)` を張る。これは
`LIGHT_SETTLE_MS` 後に**通信せず**既知状態へ描き戻すだけのタイマーで、
push が先に来れば同じ値の再描画になるだけ（冪等）。exec ゼロという push の
価値を捨てずに表示の固着を防ぐ。

> **その後の訂正:** この「通信しない再描画」だけでは、push が永久に来ない
> デバイス（node_id 未設定 / `mat` が古い / `matd` 停止 / node_id ドリフト）で
> 押下前の状態を確定値として出してしまう。最終レビューを受けて、空振りしたら
> 追いつき取得へ落ちる見張りに変えた（上記 9）。

### 4. `node_id` 欠落の warn は `[push]` があるときだけ出す

設計書は「`kind = "light"` で `node_id` が無いデバイスは…起動時に warn を出す」と
条件を付けていないが、`[push]` 未設定の環境では `node_id` に意味がないので、
そこで warn を出すのは全ユーザーに対する純粋なノイズになる。`[push]` があるとき
だけ出す（＝「push を使うと言ったのに突合キーが無い」という本物の設定漏れのときだけ）。

### 5. デプロイ前提: jarvis の `mat` は `listen` を持つ版であること

`mat listen` は mat 0.25.0 で入った（`recovered` フィールドは 1.2.0）。
開発機に入っている `mat` は 0.5.0 で `listen` を持たない。jarvis 側の `mat` が
古い場合、listener は spawn 直後に「unrecognized subcommand」で落ち、backoff で
再試行し続ける（**GET state は read フォールバックで動き続ける** — 機能が壊れる
のではなく push が効かないだけ）。`[push]` を config に入れるのは mat を上げてから。

## 実装中に入った修正（Task 4 のレビュー結果）

計画のコードは scratchpad で compile / test 検証済みだったが、**並行性の穴は
テストが通ることでは見つからない。** Task 4 のレビューで 2 件の Important が
出て、以下を追加した（詳細は `.superpowers/sdd/task-4-report.md`）:

1. **接続世代（`Inner.generation`）。** `baseline` は書き込み時点の
   `connected` しか見ていなかったため、read（最大 15s）が listener の
   断・再接続（backoff 最短 1s）を跨ぐと、**断の前に読んだ値**が primed /
   `stale: false` として出てしまう。read を始める前の世代を覚え、跨いだ戻りは
   採用しない。`baseline` は `baseline(&self, device, state, generation)` の
   3 引数になった（Task 5 以降もこのシグネチャ）。
2. **再ベースライン依頼の畳み込み。** listener が連続して落ちると
   「1 周ぶんの read（N デバイス × 最大 exec timeout）」が unbounded channel に
   滞留し、不調な matd を余計に叩き続ける。sweep の前に溜まった依頼を drain する。
3. **stderr をバイトで drain。** 行読みは不正な UTF-8 1 バイトで降りる。
   降りるとパイプが埋まって子が止まり、stdout が EOF にならないまま
   「生きているのに何も届かない」listener になる（鮮度モデルが存在しないと
   仮定している状態）。
4. **stdout 読み取り失敗を EOF 扱いに。** 起動失敗と混同したログを出さない。

再レビューで、この 4 番目の修正**そのもの**が欠陥だと判明した（計画側の
処方ミス）。stdout ループを抜けて `child.wait()` に落ちても、子が生きていれば
永久に戻らない — tokio の `Child::wait` が閉じるのは stdin だけで、
`mat listen` は自分から終わらない。戻らないと `set_connected(false)` に
到達せず、`connected` のまま「生きているのに何も届かない」listener になり、
primed 値を `stale: false` として出し続ける。直そうとした欠陥より悪い。
2 巡目で以下を追加した:

5. **stdout 終了後の待ちを有界化して kill。** 猶予（`CHILD_EXIT_GRACE = 2s`）を
   超えて残っていたら `start_kill()`。通常経路では本物の終了コードを観測した
   まま（matd 不在の exit 13 は運用の手がかり）。不正な UTF-8 を出して自分では
   終わらない子で回帰テストを追加した（修正前は 20s ハング、修正後 2.00s で通る）。
6. `baseline` を `tracks()` で門番して管理外の slot を作らない。
7. `set_connected` は復帰でも slot を捨てる（断中に書かれた値が復帰で primed に
   昇格しないことを構造的に保証する）。
8. listener の起動を bind 成功後へ移動（bind 失敗の `exit(1)` では
   デストラクタが走らず子が取り残される）。
9. **settle タイマーを push の見張りに変えた。** `sseOpen` は「ブラウザとの
   socket が生きているか」しか語らない。node_id 未設定・`mat` が古い・`matd` 停止・
   node_id ドリフトではイベントが永久に来ないので、それを根拠に押下後の確認を
   やめると、物理的に点いているライトを「消灯」と言い切ってしまう（原則 7 の反転、
   かつ本ブランチ以前からの退行）。`LIGHT_SETTLE_MS` の間に対象デバイスの
   イベントが来なければ従来の追いつき取得へ落ちる。健全な経路の費用はゼロ
   （先にイベントが来て見張りが解除される）。設計書 §クライアントの
   「SSE 接続中は `scheduleLightCatchup` を張らない」はこの形に読み替える。
10. **操作 POST は送信の**前**に基準値を落とす。** 送信できたことは確認では
    ないので押下前の値を primed のまま出せない。かつ exec の後に落とすと、
    exec 中に届いた push を消してしまい、クライアントの追いつき read が必ず
    走る。同じ代表ノードを共有するデバイス（グループカードとメンバー）は
    まとめて落とす — `apply` がイベントを配る範囲と揃える。

**教訓:** 計画のコードは「コンパイルとテストが通る」ところまでしか検証されて
いない。サブプロセスのライフサイクルと並行性は、レビューで読んで初めて
穴が出る。

## File Structure

| ファイル | 責務 | 変更 |
|---|---|---|
| `Cargo.toml` | tokio に `io-util`（listener の行読み）、`futures-util`（SSE ストリーム合成）を追加。`futures-util` は既に間接依存として lock 済みで、`default-features = false, features = ["std"]` なら **lock に新しい crate は 1 つも増えない** | Task 4 / 5 |
| `src/config.rs` | `[push]`（`Push { listen }`）と `Device.node_id`。検証は「`[push]` があるなら `listen` は非空」だけ | Task 1 |
| `src/normalize.rs` | 下層固有知識の追加: `normalize_onoff_value` / `state_to_onoff_value` / `read_node_id` / `PushEvent` / `parse_mat_listen_event` / `attr_key` / `ONOFF_KEY` | Task 2 / 4 |
| `src/push.rs`（新規） | `PushStore`（突合・鮮度・汎用属性マップ）と listener タスク（サブプロセス + backoff 再起動）、broadcast | Task 4 / 5 |
| `src/main.rs` | App に `push` フィールド、起動配線、再ベースライン read、GET state の三段構え、`GET /api/events` | Task 1〜5 |
| `index.html` | `EventSource("/api/events")`、`renderState` の `source`/`stale`、settle / catchup の出し分け | Task 6 |
| `config.example.toml` / `README.md` / `CLAUDE.md` | `[push]` / `node_id` / `/api/events` のドキュメント | Task 7 |

**タスク境界の根拠:** `cargo clippy -- -D warnings` は `dead_code` を error にするので、
「追加した pub 関数を次のタスクで初めて使う」形の分割ができない（追加した時点で
`function is never used` で落ちる）。したがって各タスクは**追加物の消費者を同じタスクに含む
縦の薄切り**になっている。Task 4 が最も大きいが、これが「クリーンに commit できる最小単位」
である（PushStore だけ、listener だけでは読み手がいない）。

---

### Task 1: config の `[push]` と `node_id`

**Files:**
- Modify: `src/config.rs`（`Config` / `Push` / `Device` / `ConfigError` / `validate` / tests）
- Modify: `src/main.rs`（`warn_missing_node_ids` と `main()` からの呼び出し）

**Interfaces:**
- Consumes: 既存の `Config` / `Device` / `ConfigError` / `Kind`
- Produces:
  - `pub struct config::Push { pub listen: Vec<String> }`
  - `pub push: Option<Push>` on `config::Config`
  - `pub node_id: Option<u64>` on `config::Device`
  - `config::ConfigError::EmptyPushListen`
  - `fn warn_missing_node_ids(config: &Config)` in `main.rs`

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `mod tests` の先頭（`fn write_tmp` の直前）に追加する:

```rust
    #[test]
    fn push_absent_is_none() {
        let c: Config = toml::from_str(
            r#"
            [[device]]
            name = "s1"
            get_state = ["true"]
            open = ["true"]
            close = ["true"]
            "#,
        )
        .unwrap();
        assert!(c.push.is_none(), "[push] 無しなら push 機能ごと無効");
    }

    #[test]
    fn push_listen_parses() {
        let c: Config = toml::from_str(
            r#"
            [push]
            listen = ["mat", "listen", "--count", "4294967295", "--timeout-ms", "0"]
            [[device]]
            name = "s1"
            get_state = ["true"]
            open = ["true"]
            close = ["true"]
            "#,
        )
        .unwrap();
        assert_eq!(c.push.unwrap().listen.first().unwrap(), "mat");
    }

    #[test]
    fn push_empty_listen_rejected() {
        let p = write_tmp(
            "pushempty",
            r#"
            [push]
            listen = []
            [[device]]
            name = "s1"
            get_state = ["true"]
            open = ["true"]
            close = ["true"]
            "#,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::EmptyPushListen)
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn node_id_parses_and_defaults_to_none() {
        let p = write_tmp(
            "nodeid",
            r#"
            [push]
            listen = ["true"]
            [[device]]
            name = "desk_light"
            kind = "light"
            node_id = 6
            get_state = ["true"]
            on = ["true"]
            off = ["true"]
            [[device]]
            name = "plain"
            kind = "light"
            get_state = ["true"]
            on = ["true"]
            off = ["true"]
            "#,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("desk_light").unwrap().node_id, Some(6));
        // node_id 無しの light は設定エラーにしない（そのデバイスだけ read 経路）。
        assert_eq!(cfg.find("plain").unwrap().node_id, None);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn node_id_on_non_light_is_accepted_and_ignored() {
        // light 以外では無視するだけ（既存 config を壊さない）。
        let p = write_tmp(
            "nodeidshutter",
            r#"
            [[device]]
            name = "s1"
            node_id = 3
            get_state = ["true"]
            open = ["true"]
            close = ["true"]
            "#,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("s1").unwrap().node_id, Some(3));
        std::fs::remove_file(p).ok();
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando config::tests::push`
Expected: コンパイルエラー（`no field \`push\` on type \`Config\``）

- [ ] **Step 3: config.rs に `Push` / `node_id` / 検証を実装**

`Config` に追加（`mesh` の直後）:

```rust
    /// light 状態の push 取り込み設定（任意）。未設定なら push 機能ごと無効。
    #[serde(default)]
    pub push: Option<Push>,
```

`fn default_state_ttl_ms()` の直後（`/// 複数デバイスをまとめて…` の直前）に追加:

```rust
/// light 状態の push 取り込み（`mat listen` → SSE）の設定。任意。
///
/// listen は無期限ストリームなので、one-shot exec 用の `[exec] timeout_ms` や
/// レーン直列化は通さない（通すと即座に打ち切られる）。
#[derive(Debug, Clone, Deserialize)]
pub struct Push {
    /// 長寿命の listen コマンド配列。
    pub listen: Vec<String>,
}
```

`Device` の `members` の直後（`lane` の直前）に追加:

```rust
    /// push イベントの突合キー（light 専用・任意）。`mat listen` の
    /// `node_id` と突き合わせる。機器を commission した結果決まる
    /// デプロイデータなので、実 IP・EPC と同じクラスのものとして config に置く。
    /// light 以外の kind では無視する。
    #[serde(default)]
    pub node_id: Option<u64>,
```

`ConfigError` の `EmptyMeshCommand` の直後に `EmptyPushListen,` を追加し、`Display` に:

```rust
            ConfigError::EmptyPushListen => write!(f, "push: listen が空"),
```

`validate()` の `if let Some(m) = &self.mesh { … }` ブロックの直後（`Ok(())` の直前）に:

```rust
        if let Some(p) = &self.push {
            if p.listen.is_empty() {
                return Err(ConfigError::EmptyPushListen);
            }
        }
```

既存テスト `default_label_is_name` の `Device { … }` リテラルに `node_id: None,` を
`members: vec![],` の直後へ追加する（コンパイルエラーになるので必須）。

- [ ] **Step 4: main.rs で `config.push` / `node_id` を読む（dead_code を残さない）**

`shutdown_signal()` の直後に追加:

```rust
/// `[push]` 有りのとき、node_id の設定漏れを起動時に告知する（無しなら
/// node_id は意味を持たないので黙る）。設定漏れを黙って無視しない —
/// node_id の無い light はそのデバイスだけ従来の read 経路のままになる。
fn warn_missing_node_ids(config: &Config) {
    if config.push.is_none() {
        return;
    }
    for d in &config.devices {
        if d.kind == Kind::Light && d.node_id.is_none() {
            tracing::warn!(
                device = %d.name,
                "kind=light に node_id が無い: push を使わず従来の read 経路のまま"
            );
        }
    }
}
```

`main()` の中、`let app = Arc::new(App { … })` の直前に:

```rust
    warn_missing_node_ids(&config);
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS / warning なし

- [ ] **Step 6: コミット**

```bash
git add src/config.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat(config): [push] セクションと device.node_id を追加

light 状態の push 取り込みの設定入口。[push] 未設定なら push 機能ごと無効で
既存挙動と変わらない。node_id は commission の結果決まるデプロイデータなので
実 IP・EPC と同じクラスのものとして config に置く（read から学習しない —
学習には read 成功が要り、その read こそが cold で遅い／失敗するもの）。
node_id の無い light は起動時に warn を出し、そのデバイスだけ read 経路のまま。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

### Task 2: normalize に onoff 値の写しと node_id ドリフト検出

**Files:**
- Modify: `src/normalize.rs`（`normalize_mat_onoff` のリファクタ + 2 関数追加 + tests）
- Modify: `src/main.rs`（`warn_on_node_id_drift` を `fetch_state` から呼ぶ）

**Interfaces:**
- Consumes: `config::Device.node_id`（Task 1）、既存 `normalize::State`
- Produces:
  - `pub fn normalize::normalize_onoff_value(value: &Value) -> State`
  - `pub fn normalize::read_node_id(raw: &Value) -> Option<u64>`
  - `fn warn_on_node_id_drift(device: &Device, raw: &Value)` in `main.rs`

> このタスクで追加するのは 2 つだけ: `normalize_onoff_value`（既存
> `normalize_mat_onoff` が消費する）と `read_node_id`（`warn_on_node_id_drift` が
> 消費する）。逆写像の `state_to_onoff_value` は消費者（`PushStore::baseline`）が
> できる Task 4 で入れる — ここで入れると `dead_code` で clippy が落ちる。

- [ ] **Step 1: 失敗するテストを書く**

`src/normalize.rs` の `mod tests` の先頭（`use serde_json::json;` の直後）に追加:

```rust
    #[test]
    fn onoff_value_maps_bool_only() {
        assert_eq!(normalize_onoff_value(&json!(true)), State::On);
        assert_eq!(normalize_onoff_value(&json!(false)), State::Off);
        // bool 以外は解釈しない（0 / 1 を on/off と決めつけない）。
        assert_eq!(normalize_onoff_value(&json!(1)), State::Unknown);
        assert_eq!(normalize_onoff_value(&json!("on")), State::Unknown);
        assert_eq!(normalize_onoff_value(&json!(null)), State::Unknown);
    }

    #[test]
    fn reads_node_id_for_drift_check() {
        assert_eq!(
            read_node_id(&json!({"node_id": 6, "value": true})),
            Some(6)
        );
        assert_eq!(read_node_id(&json!({"value": true})), None);
        assert_eq!(read_node_id(&json!({"node_id": "6"})), None);
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando normalize::tests::onoff_value_maps_bool_only`
Expected: FAIL（`cannot find function \`normalize_onoff_value\``）

- [ ] **Step 3: normalize.rs を実装**

既存の `normalize_mat_onoff` の本体を差し替え、直後に 2 関数を追加する
（doc コメントはそのまま残す）:

```rust
pub fn normalize_mat_onoff(raw: &Value) -> State {
    raw.get("value")
        .map(normalize_onoff_value)
        .unwrap_or(State::Unknown)
}

/// `onoff/on-off` の値 → 論理 state。bool 以外は Unknown。
pub fn normalize_onoff_value(value: &Value) -> State {
    match value {
        Value::Bool(true) => State::On,
        Value::Bool(false) => State::Off,
        _ => State::Unknown,
    }
}

/// read の戻り値から node_id を取り出す（config とのドリフト検出用）。
pub fn read_node_id(raw: &Value) -> Option<u64> {
    raw.get("node_id").and_then(Value::as_u64)
}
```

- [ ] **Step 4: main.rs でドリフト warn を出す**

`fetch_state` の `match serde_json::from_str::<Value>(&result.stdout)` の `Ok` 腕を
ブロックにして、正規化の前に warn を挟む:

```rust
    match serde_json::from_str::<Value>(&result.stdout) {
        Ok(raw) => {
            warn_on_node_id_drift(device, &raw);
            StateView {
                state: match device.kind {
                    Kind::Shutter => normalize_enl_state(&raw),
                    Kind::Light => normalize_mat_onoff(&raw),
                    Kind::Switch => normalize_enl_state(&raw),
                },
                exec: result.outcome,
                raw: Some(raw),
            }
        }
```

`fetch_state` の直後に追加:

```rust
/// 再 commission 等で config の node_id が実機とずれたら warn を出す。
/// 自己修復はしない — 設定の真実は config、という一貫性を優先する。
/// light は定期ポーリングされないので、warn がログを埋めることはない。
fn warn_on_node_id_drift(device: &Device, raw: &Value) {
    let (Some(expected), Some(actual)) = (device.node_id, normalize::read_node_id(raw)) else {
        return;
    };
    if expected != actual {
        tracing::warn!(
            device = %device.name,
            expected,
            actual,
            "config の node_id が read の戻り値と不一致（再 commission？ push イベントが突合できない）"
        );
    }
}
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS / warning なし

- [ ] **Step 6: コミット**

```bash
git add src/normalize.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat(normalize): onoff 値の写しを関数化し node_id ドリフトを warn する

normalize_mat_onoff から値 → state の写しを normalize_onoff_value として
切り出す（push の属性マップからも同じ写しを使うため）。read の戻り値の
node_id が config と食い違ったら warn を出し、再 commission で黙って
壊れないようにする（自己修復はしない — 設定の真実は config）。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

### Task 3: `StateView.exec` を `Option<ExecOutcome>` にする

走らなかった exec の成功を騙らないための下地（原則 7）。この時点では常に `Some` なので
**応答 JSON は変わらない**。純粋に機械的な差し替え。

**Files:**
- Modify: `src/main.rs`（`StateView` / `fetch_state` / `cached_state` / `run_action` / tests）

**Interfaces:**
- Produces: `StateView { state: DeviceState, exec: Option<ExecOutcome>, raw: Option<Value> }`

- [ ] **Step 1: 既存テストを新しい型に合わせる（これが失敗するテスト）**

`src/main.rs` の `device_exec_times_out_and_maps_to_timeout_outcome` の 1 行を変える:

```rust
        assert_eq!(view.exec, Some(ExecOutcome::Timeout));
```

さらに「shutter の応答の形が変わっていない」ことを釘打つテストを
`devices_list_has_members_only_when_present` の直前に追加する:

```rust
    /// shutter は本設計の対象外 — 応答の形が変わらないことの釘打ち。
    /// （Task 4 で source / stale の不在アサートを足す）
    #[tokio::test]
    async fn shutter_state_response_shape_unchanged() {
        let (st, v) = call("GET", "/api/devices/shutter/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "open");
        assert_eq!(v["exec"], "success");
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando device_exec_times_out`
Expected: FAIL（`expected \`ExecOutcome\`, found \`Option<ExecOutcome>\``）

- [ ] **Step 3: `StateView` と全代入箇所を直す**

`StateView` の定義:

```rust
/// state テンプレを exec → 正規化した結果。
#[derive(Serialize, Clone)]
struct StateView {
    /// 正規化された状態。shutter: open | closed | … / light: on | off。想定外は unknown。
    state: DeviceState,
    /// get_state の exec 結果（成否を正直に出す）。push 由来の即答では
    /// exec が走っていないので省略する — 走らなかった exec の成功を騙らない。
    #[serde(skip_serializing_if = "Option::is_none")]
    exec: Option<ExecOutcome>,
    /// 下層の生 JSON（パースできた場合のみ。デバッグ用）。
    raw: Option<Value>,
}
```

`fetch_state` の 3 箇所の `exec: result.outcome,` を `exec: Some(result.outcome),` にする。

`cached_state` の中:

```rust
            let cacheable = view.exec == Some(ExecOutcome::Success);
```

`run_action` の中:

```rust
    if state.exec == Some(ExecOutcome::Success) {
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS / warning なし

- [ ] **Step 5: コミット**

```bash
git add src/main.rs
git commit -m "$(cat <<'EOF'
refactor(api): StateView.exec を Option 化（走らなかった exec を騙らない）

push 由来の即答では get_state の exec が 1 回も走らない。そこに success を
入れるのは嘘なので、Option にして省略できるようにする。この時点では常に
Some なので shutter / switch の応答 JSON は変わらない（釘打ちテスト付き）。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

### Task 4: `src/push.rs` — PushStore と listener、GET state の三段構え

このタスクが本設計の核。`PushStore`（突合・鮮度・汎用属性マップ）、listener タスク
（サブプロセス + backoff 再起動）、起動配線、再ベースライン read、GET state の
push 優先を**まとめて**入れる。分割すると `dead_code` で clippy が落ちる（読み手のいない
store／書き手のいない読み手になる）ため、これがクリーンに commit できる最小単位。

broadcast / SSE は Task 5。ここでは store は「書いて読むだけ」で通知はしない。

**Files:**
- Modify: `Cargo.toml`（tokio に `io-util`）
- Modify: `src/normalize.rs`（`PushEvent` / `parse_mat_listen_event` / `name_or_number` / `attr_key` / `ONOFF_KEY` / `state_to_onoff_value` + tests）
- Create: `src/push.rs`
- Modify: `src/main.rs`（`mod push;` / App の `push` / `start_push` / `rebaseline_push_devices` / `push_state` / `StateView` の `source`・`stale` / tests）

**Interfaces:**
- Consumes: `config::{Config, Kind, Push}`、`normalize::State`、`main::fetch_state`、`exec::ExecOutcome`
- Produces:
  - `pub struct normalize::PushEvent { pub node_id: u64, pub cluster: String, pub attribute: String, pub value: Value }`
  - `pub fn normalize::parse_mat_listen_event(line: &str) -> Option<PushEvent>`
  - `pub fn normalize::attr_key(cluster: &str, attribute: &str) -> String`
  - `pub const normalize::ONOFF_KEY: &str`
  - `pub fn normalize::state_to_onoff_value(state: State) -> Option<Value>`
  - `pub struct push::PushStore` with `new(&Config) -> Self` / `tracks(&str) -> bool` / `primed_state(&str) -> Option<State>` / `baseline(&str, State)` / `apply(&PushEvent) -> bool` / `set_connected(bool)`
  - `pub const push::SOURCE_PUSH: &str` / `pub const push::SOURCE_READ: &str`
  - `pub async fn push::run_listener(cmd: Vec<String>, store: Arc<PushStore>, rebaseline: mpsc::UnboundedSender<()>)`
  - `pub(crate) fn push::ingest(store: &PushStore, line: &str)`
  - `StateView { state, exec, raw, source: Option<&'static str>, stale: Option<bool> }`
  - `push: Option<Arc<push::PushStore>>` on `main::App`

- [ ] **Step 1: normalize の失敗するテストを書く**

`src/normalize.rs` の `mod tests` の先頭に追加:

```rust
    #[test]
    fn parses_listen_event() {
        let ev = parse_mat_listen_event(
            r#"{"timestamp":"2026-07-20T21:00:00+09:00","node_id":5,"endpoint":1,"cluster":"onoff","attribute":"on-off","value":true,"priming":false,"recovered":false}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            PushEvent {
                node_id: 5,
                cluster: "onoff".into(),
                attribute: "on-off".into(),
                value: json!(true),
            }
        );
    }

    #[test]
    fn parses_listen_event_with_numeric_ids() {
        // 未知 cluster / attribute は数値のまま来る（mat の read と同じ規律）。
        let ev = parse_mat_listen_event(
            r#"{"node_id":9,"cluster":1234,"attribute":7,"value":42}"#,
        )
        .unwrap();
        assert_eq!(ev.cluster, "1234");
        assert_eq!(ev.attribute, "7");
        assert_eq!(ev.value, json!(42));
    }

    #[test]
    fn rejects_malformed_listen_lines() {
        for line in [
            "",
            "not json",
            "[]",
            // ack 行（mat は読み捨てるが、来ても落ちない）
            r#"{"listening":true}"#,
            // value 欠落
            r#"{"node_id":5,"cluster":"onoff","attribute":"on-off"}"#,
            // node_id 欠落
            r#"{"cluster":"onoff","attribute":"on-off","value":true}"#,
            // node_id が数値でない
            r#"{"node_id":"5","cluster":"onoff","attribute":"on-off","value":true}"#,
            // node_id が負
            r#"{"node_id":-1,"cluster":"onoff","attribute":"on-off","value":true}"#,
            // cluster が文字列でも数値でもない
            r#"{"node_id":5,"cluster":null,"attribute":"on-off","value":true}"#,
        ] {
            assert!(
                parse_mat_listen_event(line).is_none(),
                "受けてはいけない行: {line}"
            );
        }
    }

    #[test]
    fn state_to_onoff_value_is_the_inverse() {
        assert_eq!(state_to_onoff_value(State::On), Some(json!(true)));
        assert_eq!(state_to_onoff_value(State::Off), Some(json!(false)));
        assert_eq!(state_to_onoff_value(State::Unknown), None);
        assert_eq!(state_to_onoff_value(State::Open), None);
    }

    #[test]
    fn attr_key_joins_cluster_and_attribute() {
        assert_eq!(attr_key("onoff", "on-off"), ONOFF_KEY);
        assert_eq!(
            attr_key("levelcontrol", "current-level"),
            "levelcontrol/current-level"
        );
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando normalize::tests::parses_listen_event`
Expected: FAIL（`cannot find function \`parse_mat_listen_event\``）

- [ ] **Step 3: normalize.rs に listen イベントの知識を実装**

Task 2 で入れた `read_node_id` の直後に追加:

```rust
/// PushStore の汎用マップのキー（`"onoff/on-off"` 形）。
/// 明るさ・色を足すときはここに別の属性が増えるだけ。
pub fn attr_key(cluster: &str, attribute: &str) -> String {
    format!("{cluster}/{attribute}")
}

/// state に写す属性（Matter 固有の知識）。
pub const ONOFF_KEY: &str = "onoff/on-off";

/// 論理 state → `onoff/on-off` の値表現。read で確定した基準値を push の
/// 汎用マップに載せるための逆写像（On / Off 以外は載せない）。
pub fn state_to_onoff_value(state: State) -> Option<Value> {
    match state {
        State::On => Some(Value::Bool(true)),
        State::Off => Some(Value::Bool(false)),
        _ => None,
    }
}

/// `mat listen` の 1 イベント。下層固有の形をここで吸収し、push.rs は
/// この構造体しか見ない（設計原則 4）。
#[derive(Debug, Clone, PartialEq)]
pub struct PushEvent {
    pub node_id: u64,
    pub cluster: String,
    pub attribute: String,
    pub value: Value,
}

/// listen の 1 行 → PushEvent。
///
/// mat listen の実出力例:
/// `{"timestamp":"...","node_id":5,"endpoint":1,"cluster":"onoff",
///   "attribute":"on-off","value":true,"priming":false,"recovered":false}`
///
/// cluster / attribute は既知 ID なら chip-tool 記法名、未知なら数値で来るので
/// どちらも受けて文字列キーへ正規化する。node_id / cluster / attribute / value
/// のいずれかが欠けた行・ack 行（`{"listening":true}`）・壊れた JSON は None。
/// `priming` / `recovered` は区別しない — どちらもその時点の実値を運ぶ。
pub fn parse_mat_listen_event(line: &str) -> Option<PushEvent> {
    let raw: Value = serde_json::from_str(line).ok()?;
    Some(PushEvent {
        node_id: raw.get("node_id")?.as_u64()?,
        cluster: name_or_number(raw.get("cluster")?)?,
        attribute: name_or_number(raw.get("attribute")?)?,
        value: raw.get("value")?.clone(),
    })
}

/// cluster / attribute の値（chip-tool 記法名 or 数値 ID）を文字列キーへ。
fn name_or_number(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
```

- [ ] **Step 4: tokio に `io-util` を足す**

`Cargo.toml`:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "sync", "signal", "io-util"] }
```

- [ ] **Step 5: `src/push.rs` を作る（PushStore + listener。broadcast はまだ無い）**

```rust
//! light 状態の push 取り込み（`mat listen` → in-memory）。
//!
//! 下層固有のイベント形の知識は `normalize.rs` に閉じ、このモジュールは
//! 「行を受け取り正規化関数に渡し、store を更新する」だけの下層非依存な
//! 機械に保つ（設計原則 4）。
//!
//! listener は `run_bounded` を通さない。あれは one-shot exec 用の
//! 「レーン直列化 + timeout」であり、無期限ストリームに適用すると即座に
//! 打ち切られる。listen は matd 経由で 3610 を掴まないためレーンも不要
//! （CLAUDE.md 原則 5 の「mat は matd が並行を捌くのでレーン不要」と同じ理由）。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::config::{Config, Kind};
use crate::normalize::{self, PushEvent, State};

/// 値の出どころ。UI が「いま何を根拠に表示しているか」を隠さないため（原則 7）。
pub const SOURCE_PUSH: &str = "push";
pub const SOURCE_READ: &str = "read";

/// listener 再起動の待ち（指数 backoff）。
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// これだけ生きていたなら一時的な事故とみなして backoff を初期値へ戻す
/// （戻さないと一度荒れたあと永久に上限で待つことになる）。
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);

/// デバイス 1 台の push 状態。
#[derive(Default)]
struct Slot {
    /// `"cluster/attribute"` → 最新値の汎用マップ。今 state に写すのは
    /// `onoff/on-off` だけ。明るさ・色は読み出す属性を足すだけで済む。
    attrs: HashMap<String, Value>,
}

impl Slot {
    /// 汎用マップから論理 state を導く。値が無い / 解釈できない値なら None
    /// （＝基準値未確立。呼び出し側は read で確定する）。
    fn state(&self) -> Option<State> {
        match self
            .attrs
            .get(normalize::ONOFF_KEY)
            .map(normalize::normalize_onoff_value)
        {
            Some(State::Unknown) | None => None,
            Some(s) => Some(s),
        }
    }
}

struct Inner {
    /// listener が生きているか。
    connected: bool,
    /// device 名 → slot。
    slots: HashMap<String, Slot>,
}

/// node_id → 論理デバイスの突合と、デバイスごとの最新属性値の in-memory 保持。
///
/// 鮮度は TTL で腐らせない。静止したライトの状態は勝手に変わらず、変われば
/// イベントが来る。信頼できるかどうかは listener が生きているかだけで決まる。
pub struct PushStore {
    /// 突合表。1 つの node_id が複数の論理デバイス（グループカードと
    /// そのメンバー等）の代表ノードになりうるので Vec で持つ。
    by_node: HashMap<u64, Vec<String>>,
    /// push 管理下のデバイス名（config 記載順）。
    tracked: Vec<String>,
    inner: Mutex<Inner>,
}

impl PushStore {
    /// config の `kind = "light"` かつ `node_id` ありのデバイスだけを対象に作る。
    pub fn new(config: &Config) -> Self {
        let mut by_node: HashMap<u64, Vec<String>> = HashMap::new();
        let mut tracked = Vec::new();
        for d in &config.devices {
            // node_id は light 以外の kind では無視する。
            if d.kind != Kind::Light {
                continue;
            }
            let Some(node_id) = d.node_id else { continue };
            by_node.entry(node_id).or_default().push(d.name.clone());
            tracked.push(d.name.clone());
        }
        PushStore {
            by_node,
            tracked,
            inner: Mutex::new(Inner {
                connected: false,
                slots: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("push store poisoned")
    }

    /// このデバイスが push 管理下か。
    pub fn tracks(&self, device: &str) -> bool {
        self.tracked.iter().any(|n| n == device)
    }

    /// primed（listener 接続中 かつ 基準値確立済み）なら push 値を返す。
    /// これが Some なら GET state は exec ゼロで即答できる。
    pub fn primed_state(&self, device: &str) -> Option<State> {
        let inner = self.lock();
        if !inner.connected {
            return None;
        }
        inner.slots.get(device)?.state()
    }

    /// listener の接続状態を切り替える。false にすると全デバイスの基準値を
    /// 捨てる（切れていた間に状態が変化した可能性があり、`mat listen` は
    /// 新規クライアント接続へ priming を replay しないため、再 read が唯一の
    /// 正しい復旧手段）。
    pub fn set_connected(&self, connected: bool) {
        let mut inner = self.lock();
        inner.connected = connected;
        if !connected {
            inner.slots.clear();
        }
    }

    /// read で確定した値を基準値として格納する（primed 化）。
    ///
    /// 基準値は listener が生きている間だけ意味を持つので、断中は何もしない
    /// （断中の read 結果は呼び出し元の GET state 応答として直接返る）。
    pub fn baseline(&self, device: &str, state: State) {
        let Some(value) = normalize::state_to_onoff_value(state) else {
            return;
        };
        let mut inner = self.lock();
        if !inner.connected {
            return;
        }
        Self::update(&mut inner, device, normalize::ONOFF_KEY.to_string(), value);
    }

    /// listener からの 1 イベントを取り込む。突合できる論理デバイスが
    /// 無ければ false（家には mando 管理外の Matter ノードが多数いる）。
    pub fn apply(&self, ev: &PushEvent) -> bool {
        let Some(devices) = self.by_node.get(&ev.node_id) else {
            return false;
        };
        let devices = devices.clone();
        let key = normalize::attr_key(&ev.cluster, &ev.attribute);
        let mut inner = self.lock();
        for device in &devices {
            Self::update(&mut inner, device, key.clone(), ev.value.clone());
        }
        true
    }

    /// 属性を 1 つ書く。
    fn update(inner: &mut Inner, device: &str, key: String, value: Value) {
        inner
            .slots
            .entry(device.to_string())
            .or_default()
            .attrs
            .insert(key, value);
    }
}

/// listen サブプロセスを回し続ける。落ちたら指数 backoff で再起動し、
/// そのたび全デバイスの基準値を捨てて再ベースライン read を依頼する
/// （read は購読の誘発も兼ね、これが cold-start を解消する）。
pub async fn run_listener(
    cmd: Vec<String>,
    store: Arc<PushStore>,
    rebaseline: mpsc::UnboundedSender<()>,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let started = Instant::now();
        match run_once(&cmd, &store, &rebaseline).await {
            Ok(status) => tracing::warn!(code = ?status.code(), "push listener が終了した"),
            Err(e) => tracing::warn!(error = %e, "push listener を起動できない"),
        }
        store.set_connected(false);
        if started.elapsed() >= BACKOFF_RESET_AFTER {
            backoff = BACKOFF_MIN;
        }
        tracing::info!(wait_ms = backoff.as_millis() as u64, "push listener を再起動する");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// listen を 1 回起動し、stdout が終わる（＝プロセスが落ちる）まで読み続ける。
async fn run_once(
    cmd: &[String],
    store: &Arc<PushStore>,
    rebaseline: &mpsc::UnboundedSender<()>,
) -> std::io::Result<std::process::ExitStatus> {
    let (program, args) = cmd.split_first().expect("validated non-empty command");
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // タスクが drop されたとき子プロセスを残さない。
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // stderr は読み捨てないとパイプが埋まって子が止まる。診断は debug に残す。
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(line = %line, "push listener stderr");
        }
    });

    // ack を待たず接続扱いにする（起動直後に落ちれば呼び出し側が戻す）。
    // 接続扱いを先にしないと、直後の再ベースライン read が基準値を
    // 格納できない（baseline は断中に何もしない）。
    store.set_connected(true);
    let _ = rebaseline.send(());

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        ingest(store, &line);
    }
    child.wait().await
}

/// 1 行を store へ反映する。壊れた行・管理外 node はその行だけ捨て、
/// ストリームは継続する（部分的な破損で全体を落とさない）。
pub(crate) fn ingest(store: &PushStore, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    match normalize::parse_mat_listen_event(line) {
        Some(ev) => {
            if !store.apply(&ev) {
                tracing::debug!(node_id = ev.node_id, "push: 管理外 node のイベント");
            }
        }
        None => tracing::debug!(line = %line, "push: 解釈できないイベント行"),
    }
}
```

- [ ] **Step 6: `src/push.rs` の `mod tests` を書く**

`src/push.rs` の末尾に追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// node 5 が living_lights と living_south_light の代表、node 6 が desk_light。
    /// plain は node_id 無しなので push 管理外（従来の read 経路）。
    const CFG: &str = r##"
        [push]
        listen = ["true"]
        [[device]]
        name = "living_lights"
        kind = "light"
        node_id = 5
        members = ["living_south_light"]
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
        [[device]]
        name = "living_south_light"
        kind = "light"
        node_id = 5
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
        [[device]]
        name = "desk_light"
        kind = "light"
        node_id = 6
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
        [[device]]
        name = "plain"
        kind = "light"
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
    "##;

    fn store() -> PushStore {
        let cfg: Config = toml::from_str(CFG).unwrap();
        PushStore::new(&cfg)
    }

    /// 接続済み（listener 生存）の store。
    fn connected_store() -> PushStore {
        let s = store();
        s.set_connected(true);
        s
    }

    fn onoff_event(node_id: u64, on: bool) -> PushEvent {
        PushEvent {
            node_id,
            cluster: "onoff".into(),
            attribute: "on-off".into(),
            value: json!(on),
        }
    }

    #[test]
    fn tracks_only_lights_with_node_id() {
        let s = store();
        assert!(s.tracks("living_lights"));
        assert!(s.tracks("desk_light"));
        assert!(!s.tracks("plain"), "node_id 無しは push 管理外");
        assert!(!s.tracks("ghost"));
    }

    #[test]
    fn event_primes_all_devices_sharing_a_node() {
        let s = connected_store();
        assert!(s.apply(&onoff_event(5, true)));
        // 1 つの node_id が複数の論理デバイスの代表になりうる。
        assert_eq!(s.primed_state("living_lights"), Some(State::On));
        assert_eq!(s.primed_state("living_south_light"), Some(State::On));
        assert_eq!(s.primed_state("desk_light"), None);
    }

    #[test]
    fn unknown_node_is_ignored() {
        let s = connected_store();
        assert!(!s.apply(&onoff_event(99, true)), "管理外 node は false");
        assert_eq!(s.primed_state("living_lights"), None);
    }

    #[test]
    fn unprimed_while_disconnected_even_with_a_value() {
        let s = connected_store();
        s.apply(&onoff_event(6, true));
        assert_eq!(s.primed_state("desk_light"), Some(State::On));
        // 再接続時は全 light を unprimed に落として read で再ベースラインする。
        s.set_connected(false);
        assert_eq!(s.primed_state("desk_light"), None);
        s.set_connected(true);
        assert_eq!(
            s.primed_state("desk_light"),
            None,
            "再接続しただけで古い値を primed に戻してはいけない"
        );
    }

    #[test]
    fn baseline_primes_and_is_ignored_while_disconnected() {
        let s = store();
        s.baseline("desk_light", State::On);
        assert_eq!(s.primed_state("desk_light"), None, "断中は基準値を持たない");
        s.set_connected(true);
        s.baseline("desk_light", State::On);
        assert_eq!(s.primed_state("desk_light"), Some(State::On));
        // 解釈できない state は基準値にしない。
        s.baseline("living_lights", State::Unknown);
        assert_eq!(s.primed_state("living_lights"), None);
    }

    #[test]
    fn other_attributes_do_not_become_state() {
        let s = connected_store();
        // 明るさ・色の属性は汎用マップに入るが state には写らない。
        s.apply(&PushEvent {
            node_id: 6,
            cluster: "levelcontrol".into(),
            attribute: "current-level".into(),
            value: json!(120),
        });
        assert_eq!(s.primed_state("desk_light"), None);
        s.apply(&onoff_event(6, false));
        assert_eq!(s.primed_state("desk_light"), Some(State::Off));
    }

    #[tokio::test]
    async fn run_once_streams_lines_and_asks_for_rebaseline() {
        let s = Arc::new(store());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let script = concat!(
            r#"printf '{"node_id":6,"cluster":"onoff","attribute":"on-off","value":true}\n'; "#,
            r#"printf 'garbage\n'; "#,
            r#"printf '{"node_id":6,"cluster":"onoff","attribute":"on-off","value":false}\n'"#,
        );
        let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        let status = run_once(&cmd, &s, &tx).await.unwrap();
        assert!(status.success());
        assert!(rx.try_recv().is_ok(), "接続時に再ベースラインを依頼する");
        assert_eq!(
            s.primed_state("desk_light"),
            Some(State::Off),
            "壊れた行を挟んでもストリームは続く"
        );
    }

    #[tokio::test]
    async fn run_once_reports_spawn_failure() {
        let s = Arc::new(store());
        let (tx, _rx) = mpsc::unbounded_channel();
        let cmd = vec!["__mando_no_such_binary__".to_string()];
        assert!(run_once(&cmd, &s, &tx).await.is_err());
    }

    #[test]
    fn ingest_drops_bad_lines_and_keeps_going() {
        let s = connected_store();
        for line in [
            "",
            "   ",
            "not json",
            r#"{"listening":true}"#,
            r#"{"node_id":6,"cluster":"onoff","attribute":"on-off"}"#,
            r#"{"cluster":"onoff","attribute":"on-off","value":true}"#,
        ] {
            ingest(&s, line);
        }
        assert_eq!(s.primed_state("desk_light"), None, "壊れた行で state を作らない");
        // 壊れた行の後も正常な行は取り込める。
        ingest(
            &s,
            r#"{"timestamp":"t","node_id":6,"endpoint":1,"cluster":"onoff","attribute":"on-off","value":true,"priming":false,"recovered":false}"#,
        );
        assert_eq!(s.primed_state("desk_light"), Some(State::On));
    }

    #[test]
    fn ingest_takes_priming_and_recovered_events() {
        let s = connected_store();
        ingest(
            &s,
            r#"{"node_id":6,"cluster":"onoff","attribute":"on-off","value":true,"priming":true,"recovered":false}"#,
        );
        assert_eq!(
            s.primed_state("desk_light"),
            Some(State::On),
            "priming も recovered もその時点の実値を運ぶので受ける"
        );
    }
}
```

- [ ] **Step 7: main.rs を配線する**

`mod normalize;` の直後に `mod push;` を追加。

`App` の `mesh_job` の直後にフィールドを追加:

```rust
    /// light 状態の push ストア（[push] 未設定なら None）。
    /// 永続化しない — mando 再起動で unprimed から始めてよい。
    push: Option<Arc<push::PushStore>>,
```

`main()`: Task 1 で入れた `warn_missing_node_ids(&config);` の直後から差し替える:

```rust
    warn_missing_node_ids(&config);

    let store = config
        .push
        .as_ref()
        .map(|_| Arc::new(push::PushStore::new(&config)));

    let app = Arc::new(App {
        config,
        executor: Executor::new(),
        graph_executor: Executor::new(),
        mesh_executor: Executor::new(),
        state_cache: cache::Cache::default(),
        mesh_job: std::sync::Mutex::new(MeshJob::default()),
        push: store.clone(),
    });

    if let Some(store) = store {
        start_push(app.clone(), store);
    }

    let router = router(app.clone());
```

`warn_missing_node_ids` の直後に追加:

```rust
/// listener と再ベースライン受け口を起動する。どちらも起動をブロックしない。
fn start_push(app: Shared, store: Arc<push::PushStore>) {
    let listen = app
        .config
        .push
        .as_ref()
        .expect("start_push は [push] 有りでのみ呼ぶ")
        .listen
        .clone();

    // 再ベースライン read は既存の executor / lane / timeout の枠内で行う。
    // listener 側をブロックしないよう別タスクで受ける。
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let rebased = app.clone();
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            rebaseline_push_devices(rebased.clone()).await;
        }
    });

    tokio::spawn(async move { push::run_listener(listen, store, tx).await });
}

/// push 管理下の全 light の基準値を read で確定する。購読の誘発も兼ねるので
/// 起動時・listener 再接続時に必ず 1 周する（cold-start はこの read で解ける）。
/// 逐次に回す — 一斉に CASE を張らせない。
async fn rebaseline_push_devices(app: Shared) {
    let Some(store) = app.push.clone() else {
        return;
    };
    for device in &app.config.devices {
        if !store.tracks(&device.name) {
            continue;
        }
        let view = fetch_state(&app, device).await;
        if view.exec == Some(ExecOutcome::Success) {
            store.baseline(&device.name, view.state);
            tracing::debug!(device = %device.name, state = ?view.state, "push 基準値を確定");
        } else {
            tracing::warn!(
                device = %device.name,
                outcome = ?view.exec,
                "push 基準値の read に失敗（unprimed のまま）"
            );
        }
    }
}
```

`StateView` に 2 フィールドを追加（`raw` の直後）:

```rust
    /// push 管理下の light のみ。"push" = in-memory 即答 / "read" = 下層読み。
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'static str>,
    /// push 管理下の light のみ。値を信頼できないとき true（原則 7）。
    #[serde(skip_serializing_if = "Option::is_none")]
    stale: Option<bool>,
```

`fetch_state` の 3 箇所の `StateView { … }` に `source: None, stale: None,` を足す。

`cached_state` の light 分岐を差し替える:

```rust
    if device.kind == Kind::Light {
        // push 管理下なら primed 値で即答（exec ゼロ）→ read フォールバック。
        if let Some(store) = &app.push {
            if store.tracks(&device.name) {
                return push_state(app, store, device).await;
            }
        }
        return fetch_state(app, device).await;
    }
```

`cached_state` の直後に追加:

```rust
/// push 管理下 light の state（正直さの三段構え）:
/// primed なら in-memory 即答、unprimed／listener 断なら read で確定、
/// それも失敗なら `stale: true` で正直に出す。
async fn push_state(app: &App, store: &Arc<push::PushStore>, device: &Device) -> StateView {
    if let Some(state) = store.primed_state(&device.name) {
        return StateView {
            state,
            exec: None,
            raw: None,
            source: Some(push::SOURCE_PUSH),
            stale: Some(false),
        };
    }
    let mut view = fetch_state(app, device).await;
    let ok = view.exec == Some(ExecOutcome::Success);
    if ok {
        // 確定できた値を基準値にする（＝以後は exec ゼロで即答できる）。
        store.baseline(&device.name, view.state);
    }
    view.source = Some(push::SOURCE_READ);
    view.stale = Some(!ok);
    view
}
```

`mod tests` の既存 App リテラル（`test_app` / `app_from` / `call_with_cfg` /
`device_exec_times_out_and_maps_to_timeout_outcome` / `counting_app` /
`counting_light_app` の 6 箇所）に `push: None,` を追加する。

- [ ] **Step 8: main.rs のフォールバック分岐テストを書く**

`devices_list_has_members_only_when_present` の直前に追加:

```rust
    /// get_state が exec のたびに 1 文字追記する push 管理下 light を持つ App。
    /// `[push]` を持つので App.push が入るが、listener は起動しない
    /// （テストは store を直接動かす）。
    fn push_app(counter_path: &str, ok: bool) -> Shared {
        // ok: node_id 付きの mat read 出力を返す / !ok: exit 3（timeout）。
        let light_get = if ok {
            format!(
                r#"["sh", "-c", "printf x >> {counter_path}; printf '{{\"node_id\":5,\"value\":true}}'"]"#
            )
        } else {
            format!(r#"["sh", "-c", "printf x >> {counter_path}; exit 3"]"#)
        };
        let plain_get =
            format!(r#"["sh", "-c", "printf x >> {counter_path}; printf '{{\"value\":true}}'"]"#);
        let cfg: Config = toml::from_str(&format!(
            r##"
            [push]
            listen = ["true"]
            [[device]]
            name = "light"
            kind = "light"
            node_id = 5
            get_state = {light_get}
            on  = ["true"]
            off = ["true"]
            [[device]]
            name = "plain"
            kind = "light"
            get_state = {plain_get}
            on  = ["true"]
            off = ["true"]
            "##
        ))
        .unwrap();
        let store = Arc::new(push::PushStore::new(&cfg));
        Arc::new(App {
            config: cfg,
            executor: Executor::new(),
            graph_executor: Executor::new(),
            mesh_executor: Executor::new(),
            state_cache: cache::Cache::default(),
            mesh_job: std::sync::Mutex::new(MeshJob::default()),
            push: Some(store),
        })
    }

    fn tmp_counter(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("mando_push_{tag}_{}.txt", std::process::id()));
        std::fs::write(&p, "").unwrap();
        p.to_string_lossy().to_string()
    }

    /// push の価値そのものの証明: primed なら exec が 1 回も走らない。
    #[tokio::test]
    async fn primed_light_answers_without_exec() {
        let p = tmp_counter("primed");
        let app = push_app(&p, true);
        let store = app.push.clone().unwrap();
        store.set_connected(true);
        store.baseline("light", normalize::State::On);

        let (st, v) = call_on(app, "GET", "/api/devices/light/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "on");
        assert_eq!(v["source"], "push");
        assert_eq!(v["stale"], false);
        // 走らなかった exec の成功を騙らない。
        assert!(v.get("exec").is_none(), "push 即答に exec は付けない: {v:?}");
        assert_eq!(exec_count(&p), 0, "primed のとき exec は 1 回も走らない");
        std::fs::remove_file(&p).ok();
    }

    /// unprimed のときは read が 1 回走り、その結果が基準値になる。
    #[tokio::test]
    async fn unprimed_light_reads_once_then_is_primed() {
        let p = tmp_counter("unprimed");
        let app = push_app(&p, true);
        let store = app.push.clone().unwrap();
        store.set_connected(true);

        let (st, v) = call_on(app.clone(), "GET", "/api/devices/light/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "on");
        assert_eq!(v["source"], "read");
        assert_eq!(v["stale"], false);
        assert_eq!(v["exec"], "success");
        assert_eq!(exec_count(&p), 1, "unprimed のとき read は 1 回");

        // read が基準値を確立したので 2 回目は exec ゼロ。
        let (_, v2) = call_on(app, "GET", "/api/devices/light/state").await;
        assert_eq!(v2["source"], "push");
        assert_eq!(exec_count(&p), 1, "2 回目は exec しない");
        std::fs::remove_file(&p).ok();
    }

    /// listener 断のときは read フォールバックし、それも失敗なら stale で正直に出す。
    #[tokio::test]
    async fn disconnected_read_failure_is_stale() {
        let p = tmp_counter("stale");
        let app = push_app(&p, false); // get_state が exit 3（timeout）
        // set_connected を呼ばない = 起動直後 / listener 断。
        let (st, v) = call_on(app, "GET", "/api/devices/light/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "unknown");
        assert_eq!(v["source"], "read");
        assert_eq!(v["stale"], true, "信頼できない値は stale と言う");
        assert_eq!(v["exec"], "timeout");
        assert_eq!(exec_count(&p), 1);
        std::fs::remove_file(&p).ok();
    }

    /// node_id の無い light は push 管理外 = 従来の read 経路のまま。
    #[tokio::test]
    async fn light_without_node_id_keeps_read_path() {
        let p = tmp_counter("plain");
        let app = push_app(&p, true);
        let store = app.push.clone().unwrap();
        store.set_connected(true);
        store.baseline("plain", normalize::State::On); // 管理外なので効かない

        let (_, v) = call_on(app, "GET", "/api/devices/plain/state").await;
        assert_eq!(v["state"], "on");
        assert!(v.get("source").is_none(), "push 管理外に source は付けない");
        assert!(v.get("stale").is_none());
        assert_eq!(exec_count(&p), 1, "毎回 read する");
        std::fs::remove_file(&p).ok();
    }

```

さらに Task 3 で作った `shutter_state_response_shape_unchanged` の末尾に 2 行足して、
push フィールドが shutter に漏れていないことも釘打つ:

```rust
    /// shutter は本設計の対象外 — 応答の形が変わらないことの釘打ち。
    #[tokio::test]
    async fn shutter_state_response_shape_unchanged() {
        let (st, v) = call("GET", "/api/devices/shutter/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "open");
        assert_eq!(v["exec"], "success");
        assert!(v.get("source").is_none());
        assert!(v.get("stale").is_none());
    }
```

- [ ] **Step 9: テストが通ることを確認**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS / warning なし

- [ ] **Step 10: コミット**

```bash
git add Cargo.toml Cargo.lock src/normalize.rs src/push.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat(push): mat listen の常駐 listener と PushStore、GET state の三段構え

light の状態を read（pull）ではなく matd の常駐 Subscribe から push で得る。
listen を長寿命サブプロセスとして張り（run_bounded は通さない — 無期限
ストリームを打ち切ってしまう）、node_id で論理デバイスへ突合して
"cluster/attribute" → 値 の汎用マップに保持する。

鮮度は TTL で腐らせない。静止したライトの値は勝手に変わらないので、信頼は
listener が生きているかだけで決まる: primed なら exec ゼロで即答、
unprimed／断なら read で確定、それも失敗なら stale: true で正直に出す。
listener が落ちたら指数 backoff（1s→30s）で再起動し、そのたび全 light の
基準値を捨てて再ベースライン read を打つ（この read が購読を誘発し、
cold-start を解消する）。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

### Task 5: broadcast と `GET /api/events`（SSE）

**Files:**
- Modify: `Cargo.toml`（`futures-util`）
- Modify: `src/push.rs`（`StateEvent` / `tx` / `subscribe` / `snapshot` / `Slot.source` / `update` の変化検出 + tests）
- Modify: `src/main.rs`（`events` ハンドラ / route / `push_not_configured` / tests）

**Interfaces:**
- Consumes: Task 4 の `PushStore`
- Produces:
  - `pub struct push::StateEvent { pub device: String, pub state: State, pub source: &'static str, pub stale: bool }`（Serialize）
  - `PushStore::subscribe(&self) -> broadcast::Receiver<StateEvent>`
  - `PushStore::snapshot(&self) -> Vec<StateEvent>`
  - `GET /api/events`（SSE。`[push]` 未設定なら 404 `{"error":"push not configured"}`）

- [ ] **Step 1: 失敗するテストを書く（push.rs）**

`src/push.rs` の `mod tests` に追加（`other_attributes_do_not_become_state` の直後）:

```rust
    #[test]
    fn broadcasts_only_on_state_change() {
        let s = connected_store();
        let mut rx = s.subscribe();
        s.apply(&onoff_event(6, true));
        assert_eq!(
            rx.try_recv().unwrap(),
            StateEvent {
                device: "desk_light".into(),
                state: State::On,
                source: SOURCE_PUSH,
                stale: false,
            }
        );
        // 同じ値の再送では起こさない。
        s.apply(&onoff_event(6, true));
        assert!(rx.try_recv().is_err());
        // onoff を動かさない属性でも起こさない（明るさ・色が流れてきても静か）。
        s.apply(&PushEvent {
            node_id: 6,
            cluster: "levelcontrol".into(),
            attribute: "current-level".into(),
            value: json!(120),
        });
        assert!(rx.try_recv().is_err());
        // 変化したら起こす。
        s.apply(&onoff_event(6, false));
        assert_eq!(rx.try_recv().unwrap().state, State::Off);
    }

    #[test]
    fn baseline_broadcasts_with_read_source() {
        let s = connected_store();
        let mut rx = s.subscribe();
        s.baseline("desk_light", State::Off, s.generation());
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.source, SOURCE_READ);
        assert_eq!(ev.state, State::Off);
    }

    #[test]
    fn snapshot_omits_devices_without_a_baseline() {
        let s = connected_store();
        s.apply(&onoff_event(6, false));
        let snap = s.snapshot();
        assert_eq!(snap.len(), 1, "基準値のあるデバイスだけ: {snap:?}");
        assert_eq!(snap[0].device, "desk_light");
        assert_eq!(snap[0].state, State::Off);
        assert!(!snap[0].stale);
    }

    #[test]
    fn snapshot_is_empty_while_disconnected() {
        let s = connected_store();
        s.apply(&onoff_event(6, false));
        s.set_connected(false);
        assert!(s.snapshot().is_empty());
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test --bin mando push::tests::broadcasts_only_on_state_change`
Expected: FAIL（`no method named \`subscribe\``）

- [ ] **Step 3: `futures-util` を足す**

`Cargo.toml` の `toml = "0.8"` の直後に:

```toml
futures-util = { version = "0.3", default-features = false, features = ["std"] }
```

> `futures-util` は axum / tower の間接依存として既に `Cargo.lock` にある。
> `default-features = false, features = ["std"]` なら追加で入る crate はゼロ
> （`futures-core` / `futures-task` / `pin-project-lite` / `slab` はすべて lock 済み）。

- [ ] **Step 4: push.rs に broadcast を足す**

import を差し替え:

```rust
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, mpsc};
```

`SOURCE_READ` の直後に追加:

```rust
/// SSE で配る 1 件。接続直後のスナップショットも変化イベントも同じ形。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateEvent {
    pub device: String,
    pub state: State,
    pub source: &'static str,
    pub stale: bool,
}

/// broadcast バッファ。溢れた遅いクライアントは Lagged になり、次の変化
/// イベント（or 再接続時のスナップショット）で追いつく。
const BROADCAST_CAPACITY: usize = 64;
```

`Slot` に出どころを持たせる（`#[derive(Default)]` を手書き実装に変える）:

```rust
/// デバイス 1 台の push 状態。
struct Slot {
    /// `"cluster/attribute"` → 最新値の汎用マップ。今 state に写すのは
    /// `onoff/on-off` だけ。明るさ・色は読み出す属性を足すだけで済む。
    attrs: HashMap<String, Value>,
    /// 直近にこの slot を更新した出どころ。
    source: &'static str,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            attrs: HashMap::new(),
            source: SOURCE_PUSH,
        }
    }
}
```

`PushStore` に送信端を持たせる:

```rust
pub struct PushStore {
    /// 突合表。1 つの node_id が複数の論理デバイス（グループカードと
    /// そのメンバー等）の代表ノードになりうるので Vec で持つ。
    by_node: HashMap<u64, Vec<String>>,
    /// push 管理下のデバイス名（config 記載順。スナップショットの順序）。
    tracked: Vec<String>,
    inner: Mutex<Inner>,
    tx: broadcast::Sender<StateEvent>,
}
```

`PushStore::new` の末尾を差し替え:

```rust
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        PushStore {
            by_node,
            tracked,
            inner: Mutex::new(Inner {
                connected: false,
                generation: 0,
                slots: HashMap::new(),
            }),
            tx,
        }
```

> `generation` は Task 4 のレビュー修正で入った接続世代（断を跨いだ read の
> 戻りを基準値にしないための世代印）。`baseline` は
> `baseline(&self, device: &str, state: State, generation: u64)` の 3 引数で、
> この Task はその**シグネチャを変えない** — 中の `Self::update` 呼び出しを
> `self.update(..., source)` にするだけ。

`set_connected` の doc に「断そのものは broadcast しない」理由を追記する。
**本文（`generation` の bump と無条件 `slots.clear()`）は Task 4 のレビュー修正で
入ったものなので触らない** — doc コメントだけ、既にある 4 行の下に段落を足す:

```rust
    /// listener の接続状態を切り替える。切り替えのたびに全デバイスの基準値を
    /// 捨てる。断で捨てるのは必須（切れていた間に状態が変化した可能性があり、
    /// `mat listen` は新規クライアント接続へ priming を replay しないため、
    /// 再 read が唯一の正しい復旧手段）。復帰でも捨てるのは構造的な保証。
    ///
    /// 断そのものは broadcast しない — 静止したライトの値は断の間もほぼ
    /// 正しく、再接続直後の再ベースライン read が差分を必ず broadcast する。
    /// 断中に GET state を叩けば read フォールバック（失敗なら `stale: true`）
    /// で正直に出る。
```

`baseline` / `apply` の `Self::update(...)` 呼び出しを `self.update(...)` にして、
`source` を渡す:

```rust
        self.update(
            &mut inner,
            device,
            normalize::ONOFF_KEY.to_string(),
            value,
            SOURCE_READ,
        );
```

```rust
        for device in &devices {
            self.update(&mut inner, device, key.clone(), ev.value.clone(), SOURCE_PUSH);
        }
```

`update` を変化検出付きに差し替え、`subscribe` / `snapshot` を追加:

```rust
    /// 属性を 1 つ書き、導出 state が変わったときだけ broadcast する
    /// （onoff を動かさない属性の更新でクライアントを起こさない）。
    fn update(
        &self,
        inner: &mut Inner,
        device: &str,
        key: String,
        value: Value,
        source: &'static str,
    ) {
        let slot = inner.slots.entry(device.to_string()).or_default();
        let before = slot.state();
        slot.attrs.insert(key, value);
        slot.source = source;
        let after = slot.state();
        if after == before {
            return;
        }
        if let Some(state) = after {
            // 購読者ゼロなら Err。捨ててよい。
            let _ = self.tx.send(StateEvent {
                device: device.to_string(),
                state,
                source,
                stale: false,
            });
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateEvent> {
        self.tx.subscribe()
    }

    /// SSE 接続直後に送る現在スナップショット。基準値が確立していない
    /// デバイスは含めない — 「不明」で上書きして、クライアントが read で
    /// 得た正しい表示を壊さないため。
    pub fn snapshot(&self) -> Vec<StateEvent> {
        let inner = self.lock();
        if !inner.connected {
            return Vec::new();
        }
        self.tracked
            .iter()
            .filter_map(|name| {
                let slot = inner.slots.get(name)?;
                Some(StateEvent {
                    device: name.clone(),
                    state: slot.state()?,
                    source: slot.source,
                    stale: false,
                })
            })
            .collect()
    }
```

- [ ] **Step 5: main.rs に SSE ハンドラを足す**

import を差し替え:

```rust
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
```

`router()` の `/api/devices/:name/state` の直後に:

```rust
        .route("/api/events", get(events))
```

`run_mesh_job` の直後（`#[cfg(test)] mod tests` の直前）に追加:

```rust
/// [push] 未設定 → 404（機能ごと無効）。
fn push_not_configured() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"push not configured"}"#.to_string(),
    )
        .into_response()
}

/// light の状態を SSE で push する。接続直後に現在スナップショットを送り、
/// 以後は変化のたび 1 イベント。cross-tab / 別端末の操作も全画面に反映される。
async fn events(State(app): State<Shared>) -> Response {
    let Some(store) = app.push.clone() else {
        return push_not_configured();
    };
    // 購読を先に取ってからスナップショットを撮る。この順なら取りこぼさない
    // （逆順だと間のイベントが落ちる）。重複は同じ値の再描画で無害。
    let rx = store.subscribe();
    let snapshot = store.snapshot();
    let live = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((ev, rx)),
                // 遅いクライアントは取りこぼす。次の変化 or 再接続時の
                // スナップショットで追いつく。
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(missed = n, "SSE クライアントがイベントを取りこぼした");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let stream = futures_util::stream::iter(snapshot)
        .chain(live)
        .map(|ev| Event::default().json_data(&ev));
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}
```

- [ ] **Step 6: SSE のテストを書く**

`src/main.rs` の `mod tests` に追加（`shutter_state_response_shape_unchanged` の直後）:

```rust
    #[tokio::test]
    async fn events_is_404_without_push_config() {
        let (st, v) = call_with_cfg(MINIMAL_DEVICE, "GET", "/api/events").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(v["error"], "push not configured");
    }

    /// SSE ボディから次の `data:` 行の JSON を 1 件取り出す（keep-alive 行は飛ばす）。
    async fn next_sse_data(body: &mut Body) -> Value {
        for _ in 0..50 {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(3), body.frame())
                .await
                .expect("SSE frame が来ない")
                .expect("SSE ストリームが終わった")
                .unwrap();
            let Ok(bytes) = frame.into_data() else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    return serde_json::from_str(rest.trim()).expect("data 行が JSON でない");
                }
            }
        }
        panic!("data 行が来なかった");
    }

    #[tokio::test]
    async fn sse_sends_snapshot_then_change_events() {
        let p = tmp_counter("sse");
        let app = push_app(&p, true);
        let store = app.push.clone().unwrap();
        store.set_connected(true);
        store.baseline("light", normalize::State::On);

        let res = router(app.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let mut body = res.into_body();

        // 接続直後スナップショット（新しく開いたタブが即座に正しくなる）。
        let snap = next_sse_data(&mut body).await;
        assert_eq!(snap["device"], "light");
        assert_eq!(snap["state"], "on");
        assert_eq!(snap["source"], "read");
        assert_eq!(snap["stale"], false);

        // 以後は変化のたびイベント。
        store.apply(&normalize::PushEvent {
            node_id: 5,
            cluster: "onoff".into(),
            attribute: "on-off".into(),
            value: serde_json::json!(false),
        });
        let change = next_sse_data(&mut body).await;
        assert_eq!(change["device"], "light");
        assert_eq!(change["state"], "off");
        assert_eq!(change["source"], "push");
        std::fs::remove_file(&p).ok();
    }
```

- [ ] **Step 7: テストが通ることを確認**

Run:
```bash
cargo test && cargo clippy --all-targets -- -D warnings
git diff -U0 Cargo.lock
```
Expected: PASS / warning なし。`Cargo.lock` の差分は **mando の dependencies に
`futures-util` の 1 行が増えるだけ**（`[[package]]` が 1 つも増えていないこと）

- [ ] **Step 8: コミット**

```bash
git add Cargo.toml Cargo.lock src/push.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat(api): GET /api/events — light の状態変化を SSE でブラウザまで push

接続直後に全 light の現在スナップショットを 1 発送り、以後は変化のたび
1 イベント。cross-tab / 別端末の操作もライブで全画面に反映される。
broadcast は導出 state が変わったときだけ送る（明るさ・色の属性が同じ
ストリームで流れてきてもクライアントを起こさない）。source / stale を
出すのは、UI が「いま何を根拠に表示しているか」を隠さないため（原則 7）。

futures-util は axum/tower の間接依存として既に lock にあり、
default-features = false + std なので追加で入る crate はゼロ。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

### Task 6: index.html — EventSource と settle / catchup の出し分け

> **このタスク本文のコードは着手時点のもの。** レビューを経て `settle` タイマーは
> 「通信しない再描画」から **push の見張り**（空振りなら追いつき取得へ落ちる）に
> 変わっている。実際に入った形は上の「実装中に入った修正」9・10 を参照 —
> 本文に残る「通信しない」の記述はそこで読み替えること。

**Files:**
- Modify: `index.html`

**Interfaces:**
- Consumes: `GET /api/events` の `{device, state, source, stale}`、`GET state` の `source`/`stale`
- Produces: `startEvents()` / `scheduleLightSettle(name)` / `clearLightTimers(c)`、`sseOpen`

- [ ] **Step 1: 定数と状態フラグを足す**

`const LIGHT_CATCHUP_MS = 2000; …` の行を差し替え:

```javascript
const LIGHT_CATCHUP_MS = 2000; // light: SSE が切れているときの追いつき取得までの待ち（fallback）。
const LIGHT_SETTLE_MS = 4000;  // light: 押下後に push を待つ見張りの猶予（空振りなら読む）。
```

> `LIGHT_SETTLE_MS` は当初 2000 で、かつ「通信しない再描画」だった。実装中の修正 9 で
> 見張りに変わり、さらに jarvis の実測（push 到達 0.8〜3.1 秒）が 2000 の内側に
> 収まっていたため 4000 へ引き上げた。詳細は上の「実装中に入った修正」を参照。

`let busyCount = 0; …` の直後に:

```javascript
let sseOpen = false;      // /api/events が張れているか。切れている間だけ追いつき取得へ degrade。
```

- [ ] **Step 2: light タイルに settle タイマー欄を足す**

`buildLightTile` の card レコードの `catchupTimer` 行を差し替え:

```javascript
    catchupTimer: null, // 追いつき取得タイマー（SSE 断時のみ。device ごとに 1 本、連打時は張り直し）
    settleTimer: null,  // SSE 接続中の「反映中…」後始末タイマー（通信しない）
```

- [ ] **Step 3: `lightAct` を SSE 対応にし、タイマー整理を関数化**

`lightAct` 全体と、その直後に 2 つの関数を差し替え／追加する
（`function scheduleLightCatchup(name) {` の行の直前まで）:

```javascript
async function lightAct(name, verb, path, body) {
  const c = cards.get(name);
  busyCount++;
  setDeviceBusy(name, true);
  if (c) c.msgEl.textContent = verb;
  try {
    const view = await api("POST", path, body);
    const am = ACTION_MSG[view.action] || "";
    if (am) {
      clearLightTimers(c);
      // 直前の操作が残した「反映中…」を戻す。タイマーを止めた以上、
      // 他に戻す担い手がいない（light は定期ポーリングされない）。
      rerenderKnown(name);
      c.msgEl.textContent = "⚠ " + am;
      c.statusEl.classList.add("error");
    } else {
      // 成功 = 送信できただけ。中間表示にして反映を待つ。
      // 張り直す前に両方止める。sseOpen が前回操作との間で反転していると、
      // 各 schedule 関数は自分の型のタイマーしか止めないので前回のもう一方が
      // 生き残り、余計な state 読みが走る（exec ゼロの旨みを削る）。
      clearLightTimers(c);
      c.statusEl.classList.remove("error");
      c.msgEl.textContent = "";
      c.labelEl.textContent = "反映中…";
      if (sseOpen) {
        // SSE が張れているなら状態は push で降ってくる。追いつき取得はしない
        //（primed なら exec ゼロ、という push の価値をここで捨てない）。
        scheduleLightSettle(name);
      } else {
        // 切れているときだけ従来の追いつき取得へ degrade する。
        scheduleLightCatchup(name);
      }
    }
  } catch (e) {
    if (c) {
      clearLightTimers(c);
      rerenderKnown(name);
      c.msgEl.textContent = "⚠ 通信エラー"; c.statusEl.classList.add("error");
    }
  } finally {
    setDeviceBusy(name, false);
    busyCount--;
  }
}

/* 保留中の light タイマーを止める（エラー時・連打時）。 */
function clearLightTimers(c) {
  if (!c) return;
  if (c.catchupTimer) { clearTimeout(c.catchupTimer); c.catchupTimer = null; }
  if (c.settleTimer) { clearTimeout(c.settleTimer); c.settleTimer = null; }
}

/* 既知状態へ描き戻す（通信しない）。「反映中…」の後始末はここに集約する。
   exec / stale は付けない — 新たに確認したわけではないので、前回の判定を
   そのまま名乗り直さない（原則 7）。呼び出し側が warning を出す場合は
   これを呼んだ**あと**に付ける（renderState が status クラスを張り替える）。 */
function rerenderKnown(name) {
  const c = cards.get(name);
  if (c) renderState(name, { state: c.state });
}

/* SSE 接続中の「反映中…」の後始末。既に同じ状態だと変化が起きず push が
   来ないので、既知状態へ描き戻して表示を固まらせない。通信はしない。 */
function scheduleLightSettle(name) {
  const c = cards.get(name);
  if (!c) return;
  if (c.settleTimer) clearTimeout(c.settleTimer);
  c.settleTimer = setTimeout(() => {
    c.settleTimer = null;
    rerenderKnown(name);
  }, LIGHT_SETTLE_MS);
}
```

> **エラー分岐で `rerenderKnown` を呼ぶ順序が肝。** `renderState` は
> `statusEl.className` を張り替えるので、警告クラスは**そのあと**に付ける。
> これが無いと、A を押して「反映中…」が出ている間に B を押して B が失敗した
> 場合、`clearLightTimers` が A のタイマーを止めた結果ラベルを戻す担い手が
> 誰もいなくなり、**タイルが「反映中…」で永久に固まる**（light は定期
> ポーリングされないので他に直す経路が無い）。Task 6 のレビューで発覚した。

- [ ] **Step 4: `renderState` に `stale` を写し、`startEvents` を足す**

`renderState` の末尾 3 行を差し替え、直後に `startEvents` を追加:

```javascript
  // push 由来の即答には exec が付かない（走らなかった exec の成功を騙らないため）。
  // 値を信頼できないときはサーバが stale を立てるので、それも隠さず出す（原則 7）。
  const m = STATE_MSG[view.exec] || (view.stale ? "状態不明" : "");
  c.msgEl.textContent = m;
  if (m) c.statusEl.classList.add("error");
}

/* ── light 状態の push 受信（SSE）──────────────────────
   接続直後に全 light のスナップショット、以後は変化のたび 1 件。
   cross-tab / 別端末の操作もライブで反映される。EventSource は自動再接続し、
   [push] 未設定なら 404 で閉じたまま = 従来の追いつき取得のままになる。 */
function startEvents() {
  let es;
  try {
    es = new EventSource("/api/events");
  } catch (e) {
    return; // EventSource 非対応 = 従来の追いつき取得のまま
  }
  es.addEventListener("open", () => { sseOpen = true; });
  es.addEventListener("error", () => { sseOpen = false; });
  es.addEventListener("message", (e) => {
    let ev;
    try { ev = JSON.parse(e.data); } catch (_) { return; }
    if (ev && ev.device) renderState(ev.device, ev);
  });
}
```

- [ ] **Step 5: `boot()` から張る**

`boot()` の末尾を差し替え:

```javascript
  fetchHealth();
  // light が無いページでは張らない（shutter だけの構成では push は要らない）。
  if (lights.length) startEvents();
  refreshOnce();
  fetchLightStatesOnce(devices);
```

> `fetchLightStatesOnce` は**残す**。これがサーバ側の read を誘発し、
> 起動直後の再ベースラインが済んでいれば primed 経路で exec ゼロで返る。
> shutter のアクティブ窓ポーリング（`pollOnce` / `bumpPollWindow` /
> `ensurePollLoop`）には一切触らない。

- [ ] **Step 6: 構文チェックと全テスト**

Run:
```bash
python3 -c "
import re,sys
s=open('index.html').read()
m=re.search(r'<script>\n(.*)\n</script>', s, re.S)
open('/tmp/mando_app_check.js','w').write(m.group(1))
" && node --check /tmp/mando_app_check.js && rm /tmp/mando_app_check.js && cargo test
```
Expected: `node --check` が無出力で成功、`cargo test` PASS

- [ ] **Step 7: コミット**

```bash
git add index.html
git commit -m "$(cat <<'EOF'
feat(ui): light 状態を SSE で受け、SSE 接続中は追いつき取得をやめる

EventSource("/api/events") を張り、light の状態イベントで既存の renderState を
呼ぶ（描画ロジックは再利用）。接続中は追いつき取得を張らず、代わりに通信しない
settle タイマーで「反映中…」を畳む — apply は導出 state が変わったときだけ
broadcast するので、既に点いているライトを押すとイベントが来ず表示が固まる。
切れていれば従来の追いつき取得へ degrade する（degradation が正直に効く）。
shutter のアクティブ窓ポーリングには触らない。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

### Task 7: ドキュメント（config.example.toml / README / CLAUDE.md）

**Files:**
- Modify: `config.example.toml`
- Modify: `README.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: `config.example.toml` に `[push]` を追記**

`[cache] state_ttl_ms = 2000 …` の行の直後に追加:

```toml

# ── light 状態の push 取り込み（[push]、任意）──────────────────
# 未設定ならこの機能ごと無効で、既存挙動と 1 バイトも変わらない。
#
# light の on/off は groupcast（無応答マルチキャスト）なので物理は即反応するが、
# 確認 read は代表ノードへの unicast で、matd がまだ購読していないノードの
# 初回は CASE cold-start を踏んで極端に遅い（実測 3.6〜80 秒。2 発目以降は
# ~100ms）。read をやめて matd の常駐 Subscribe から push で state を得れば
# cold-start は原理的に消える。これはその pull → push 移行の設定。
#
# listen は無期限ストリームなので、[exec] timeout_ms もレーン直列化も通らない
# （通すと即座に打ち切られる）。--count は「この件数を受けたら exit 0」なので
# 実質無限（u32 上限）を渡す。--timeout-ms 0 は無期限待ち。
# 落ちたら mando が指数 backoff（1s→30s）で再起動し、そのたび全 light の
# 基準値を捨てて read で再ベースラインする（この read が購読を誘発する）。
#
# [push]
# listen = ["mat", "listen", "--count", "4294967295", "--timeout-ms", "0"]
```

`# name  = "desk_light"` の light 例の `kind  = "light"` 行の直後に追加:

```toml
# push イベントの突合キー（light 専用・任意。[push] を使うときだけ意味がある）。
# listen イベントは {"node_id": 5, ...} と数値で来るので、config 側も数値で持つ。
# 機器を commission した結果決まるデプロイデータなので、実 IP・EPC と同じクラス
# のものとして config に置く（read の戻り値からは学習しない — 学習には read 成功が
# 要り、その read こそが cold で遅い／失敗するもの）。node 番号は `mat discover` で確認。
# 未指定の light はそのデバイスだけ従来の read 経路のままになり、起動時に warn が出る。
# node_id = 5
```

「注意: light は定期ポーリングしない…」の 2 行を差し替え:

```toml
# 注意: light は定期ポーリングしない（mat 直叩きは 1 コール数秒 + exec 直列のため）。
#       [push] 未設定なら「表示時 1 回 + 操作の ~2 秒後に 1 回の追いつき取得」のみ。
#       [push] + node_id があれば状態は listen から push で降ってきて、GET state は
#       exec ゼロで即答する（UI へは /api/events の SSE で流れ、別端末の操作も反映される）。
```

`living_lights` の例の `kind  = "light"` 直後に追加:

```toml
# 代表ノードは複数の論理デバイスで共有してよい（グループカードとそのメンバーが
# 同じノードを代表にする構成）。1 件の push イベントが両方に反映される。
# node_id = 6
```

`living_south_light` の `kind  = "light"` 直後に `# node_id = 6`、
`tv_back_light` の `kind  = "light"` 直後に `# node_id = 7` を追加。

- [ ] **Step 2: `README.md` に `/api/events` を追記**

`GET  /api/devices/{name}/state` の行を差し替え:

```markdown
- `GET  /api/devices/{name}/state` — `{ state, exec, raw }`（`state` は shutter: `open|closed|…`、light: `on|off`、想定外は `unknown`）。`[push]` 管理下の light は `source`（`push`｜`read`）と `stale` が付き、push 即答では `exec` を省く（走らなかった exec の成功を騙らない）
```

`POST /api/devices/{name}/presets/{preset}` の行の直後、`state は set 後に…` の
段落の直後に追加:

```markdown
- `GET  /api/events` — light の状態変化を SSE で push（`[push]` 未設定なら 404）。接続直後に全 light の現在スナップショットを 1 発送り、以後は変化のたび `{"device","state","source","stale"}` を 1 件

> light の on/off は groupcast なので物理は即反応するが、確認 read は代表ノードへの
> unicast で、matd がまだ購読していないノードの初回は CASE cold-start で極端に遅い
> （実測 3.6〜80 秒）。`[push]` を設定すると mando は `mat listen` を長寿命
> サブプロセスとして張り、状態を in-memory に持って即答する（primed なら exec ゼロ）。
> 値の古さは TTL で腐らせない — 信頼できるかどうかは listener が生きているかだけで
> 決まる。unprimed／listener 断なら read で確定し、それも失敗なら `stale: true` で
> 正直に出す。shutter は ECHONET で push の主体がいないので従来どおり
> （set 後の同期確認 + アクティブ窓ポーリング）。
```

- [ ] **Step 3: `CLAUDE.md` を更新**

「やること（安定ミニ API）」の `GET /api/health` の行の直後に追加:

```markdown
- `GET  /api/events` — light の状態変化を SSE で push（接続直後に全 light の現在スナップショット。`[push]` 未設定なら 404）
```

原則 6 の本文末尾（「…やらない。」の直後）に引用ブロックを追加:

```markdown

   > **light の例外（Matter）:** matd は常駐 Subscribe を持つので、light には push する
   > 主体がいる。`[push]` を設定すると mando は `mat listen` を長寿命サブプロセスとして
   > 張り、状態を in-memory に持って `/api/events`（SSE）でブラウザまで push する。
   > 値の古さは TTL で腐らせず、信頼できるかどうかは **listener が生きているか**だけで
   > 決まる（primed なら exec ゼロで即答、unprimed／断なら read、それも失敗なら
   > `stale: true`）。「INF 通知のための常駐化はしない」は **echonet 限定**の話で、
   > Matter は matd が常駐購読を持つのが mat ファミリの設計なので、この非対称は意図的
   > （`docs/superpowers/specs/2026-07-25-light-push-state-design.md`）。
```

原則 7 の light 例外ブロックの末尾（`shutter は本原則どおり…` の直前）に 1 文追加:

```markdown
   > `[push]` を設定している場合、この追いつき取得は不要で状態は SSE で降ってくる
   > （追いつき取得は SSE 断時の fallback として残る。
   > `docs/superpowers/specs/2026-07-25-light-push-state-design.md`）。
```

- [ ] **Step 4: 例 config が壊れていないことを確認**

Run: `cargo test --bin mando example_config_parses_with_cache && cargo test`
Expected: PASS（`config.example.toml` は `Config` としてパースできること）

- [ ] **Step 5: コミット**

```bash
git add config.example.toml README.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: [push] / node_id / GET /api/events を config 例・README・CLAUDE.md に追記

mat listen の --count は既定 1（0 = 無限は無い）で「この件数で exit 0」なので、
実質無限（u32 上限）を渡す例を書く。原則 6 の「INF 通知のための常駐化はしない」は
echonet 限定であることを明記し、Matter で matd の常駐購読に乗る非対称が意図的で
あることを残す。

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_0143ra7dxwk5AN9Um9ffejFG
EOF
)"
```

---

## e2e 検証（jarvis 実機・人手）

コードのタスクとは別に、実機で以下を確認する。**夜間を避け、終了時は必ず元の
on/off 状態へ戻す。**

前提:

- [ ] jarvis の `mat` が `listen` を持つ版であること（`mat listen --help` が通る。mat ≥ 0.25.0）
- [ ] `matd` が常駐していること（mando の unit に `MAT_MATD_SOCKET` が渡っている）
- [ ] jarvis-iac の `roles/mando/files/config.toml` に `[push] listen` と各 light の `node_id` を追記し、Ansible で配って mando を再起動する（config はリポジトリ管理外・`~/.config/mando/`）

確認:

- [ ] mando 起動後、`curl -s localhost:8080/api/devices/living_lights/state` が
      **exec ゼロで即答**すること。判定は `source` ではなく **`exec` が付かないこと**で行う
      — 再ベースライン read 直後の出どころは `"read"` が正しい（`"push"` になるのは
      実際に listen イベントを受けたあと）。`~即答` だけだと warm read 100ms でも
      満たせてしまい、primed 経路の退行を隠す
- [ ] `journalctl --user -u mando` に「push 基準値を確定」が全 light 分出ていること。
      `node_id` 不一致の warn が出ていないこと
- [ ] 別端末（or `curl -XPOST`）で on → **開いている UI がポーリングなしで反映**されること
- [ ] 既に点いているライトの「つける」を押しても「反映中…」で固まらないこと
      （見張りが空振りして ~2 秒後に read へ落ちる。これが見張りの受け入れテスト）
- [ ] **色 / 明るさ / プリセットを押す** → onoff イベントは来ないので必ず見張りが
      空振りする経路。~2 秒で表示が畳まれ、read が 1 回だけ走ること
- [ ] `systemctl --user restart matd` → listener が再接続し、再ベースライン後に
      再び exec ゼロで即答へ戻ること（backoff のログが出る）
- [ ] **`matd` を数分止めたまま**にする → `journalctl` の sweep / backoff の間隔を見て、
      `mat read` を撒き続けていないこと（子が 3 秒生き延びるまで再ベースラインを
      頼まない仕掛けが効いているか。warn 行数も数える）
- [ ] **`matd` を止めた状態で UI からライトを押す** → 「反映中…」のあと read の結果か
      「状態不明」が出ること。**押下前の状態へ黙って戻らないこと**（原則 7 の要）
- [ ] **`node_id` を意図的に外した light** を `[push]` 有りで置く → 起動時 warn が出て、
      かつそのタイルを押しても確認が効くこと（見張りが read へ落ちる決定的ケース）
- [ ] **`node_id` を意図的に間違える** → 起動 sweep でドリフト warn が出ること。
      そのタイルを押したときの挙動も見る
- [ ] **2 つ目のタブ / 端末を開いたまま**にして cross-tab 反映を確認。スマホの通信を
      切って戻す → 再接続スナップショットで表示が直ること
- [ ] `curl -N localhost:8080/api/events` を開いたまま `matd` を再起動 → 再ベースラインの
      変化イベントが流れ、ストリームが切れないこと（subscribe→snapshot の順序と KeepAlive）
- [ ] `[push]` をコメントアウトして再起動 → `/api/events` が 404、UI は従来の
      追いつき取得で動くこと（degrade の確認）
