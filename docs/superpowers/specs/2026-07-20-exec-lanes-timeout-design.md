# exec レーン直列化 + デバイス exec timeout の設計

日付: 2026-07-20
ステータス: 設計承認済み（実装前）

## 背景（今回の障害）

2026-07-20 朝 08:32〜08:53、リビングのテープライトコントローラ（Matter, node 5/6/8）が
WiFi 上で無応答になり、matd のセッション再確立で mat コマンドが 1 回あたり数十秒
ハングした。mando の exec は全デバイス共通の `Semaphore(1)` で直列化されており、
かつデバイス exec に timeout が無いため:

1. UI のアクティブ窓ポーリングが積む mat の state 読みがレーンを塞ぐ
2. **無関係な enl（casa 経由）の操作もその後ろに並び、全デバイスが操作不能に見える**
3. ブラウザ側 fetch が諦めて切断 → handler drop → `kill_on_drop` で子プロセスが
   殺され続け、成否がログにも残らない（mando に WARN ゼロ、matd に Broken pipe 多発）

グローバル直列化の根拠は「enl が 0.0.0.0:3610 を専有 bind する」だが、これは
echonet 系（enl / casa 経由）だけの事情で、mat（matd が並行を捌く）や curl には
適用する必然がない。

## 決定

1. **レーン直列化**: exec の直列化単位をグローバル 1 本から「レーン」に分割する。
   レーンは config 宣言（`lane`）で決め、mando 本体はバックエンドを知らないまま。
2. **デバイス exec timeout**: graph query と同様に timeout で包む。デフォルト 15 秒、
   config の `[exec] timeout_ms` で可変。

## config

```toml
# 新設・省略可。デバイス exec（get_state / open / close / on / off 等）の上限
[exec]
timeout_ms = 15000        # 省略時 15000

[[device]]
name = "entrance_indirect_light"
lane = "echonet"          # 新設・省略可。同じ lane の device と直列化される
# ...
```

- `lane` 省略時は**デバイス名がレーン名**になる。つまり同一デバイスの操作と
  state 読みは互いに直列、他デバイスとは並列。
- 同じ `lane` 文字列を持つデバイス同士は Semaphore(1) で直列化される。
- graph / health は現行どおり `graph_executor`（Semaphore(1) + 30 秒 timeout）に
  相乗りのまま。今回のスコープ外。

### 実運用 config（jarvis-iac 側、リポジトリ外）

casa 経由（= enl が 3610 を bind する）の全デバイスに `lane = "echonet"` を付ける。
2026-07-20 時点ではシャッター 5 台と switch 系照明 5 台の計 10 台。mat 系・curl 系は無指定。

## 実装

### `src/exec.rs`

- `Executor::run(&self, cmd)` → `run(&self, lane: &str, cmd)`。
- 内部: `Mutex<HashMap<String, Arc<Semaphore>>>` でレーンごとに Semaphore(1) を
  遅延生成。permit 取得後に spawn する（現行と同じ流れ）。
- `graph_executor` も同じ型を使うが、呼び出し側で固定レーン名（例 `"graph"`）を
  渡すだけで従来挙動を維持する。

### timeout（`src/main.rs`）

- デバイス exec の呼び出し（get_state / 操作）を `tokio::time::timeout` で包む。
  graph の `run_graph_cmd` と同じ形の薄いラッパを共通化してよい。
- 超過は既存の `ExecOutcome::Timeout`（UI 表示は「応答なし、もう一度」）。
- future drop 時は `kill_on_drop(true)` により子プロセスが残らない（現行どおり）。
- set 送信後に timeout した場合、操作自体は届いている可能性があるが、UI は
  state 再取得で実態に追いつく（設計原則 7 のまま。楽観表示はしない）。
- timeout 値は config `[exec] timeout_ms`（省略時 15000）。

### グループ操作（`group_op`）

メンバー逐次実行は維持する。メンバーごとの操作が timeout で有界化されるため、
最悪でも `メンバー数 × timeout` で返る。並列化は今回見送り（記載順の実行順序が
保たれる方が挙動を説明しやすい）。

## ドキュメント更新

- `CLAUDE.md` 設計原則 5「exec 全体を Semaphore(1) で囲い」を「レーン直列化
  （echonet 系のみ config でレーン共有、既定はデバイス単位）+ timeout」に書き換える。
- `config.example.toml` に `lane` と `[exec]` の例・コメントを追記する。

## テスト

- 同一レーンのコマンドが直列化される（既存 interleave テストのレーン版）
- 異なるレーンのコマンドが並列に走る（sleep 重ね合わせで経過時間を検証）
- timeout 超過 → `ExecOutcome::Timeout` へのマッピング
- config パース: `lane` 省略時のデフォルト（デバイス名）、`[exec]` 省略時 15000ms
- 既存テスト（exit code マッピング等）の維持

## ロールアウト

1. 実装 + `cargo test` / `clippy`
2. cross build → despliegue skill で jarvis へ配布
3. jarvis-iac の `roles/mando/files/config.toml` に echonet 系 10 台の `lane` を追記
   → Ansible 適用（mando 再起動）
4. mando UI から mat 照明と echonet 照明を操作して動作確認

## 効果

- mat がハングしても echonet レーンは影響を受けない（今回の「enl まで死ぬ」の根絶）
- ハング自体も timeout で有界になり、UI が「応答なし、もう一度」を正直に出せる
- 3610 衝突の防止は従来どおり（echonet レーン内は直列）
