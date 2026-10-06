# loop デバイス（拍に合わせて照明を回す）設計

日付: 2026-10-06
状態: 承認済み（実装は未着手 — writing-plans → 実装）

## 背景 / 動機

Alexa で流す音楽に合わせてリビングの照明をクラブ風に動かしたい。Echo は再生音を外へ出さないので
音を取ってビート検出する道は無く、「BPM を決めて拍を刻む」擬似同期で十分それっぽく見える
（2026-10-05 実機テスト）。

実測（hogar-apps コンテナから `mat`、living_lights = 7 台、120 BPM）:

| 方式 | 結果 |
|---|---|
| groupcast（`mat group color` / `level`）を 1 拍 2〜3 発 | 全拍送信、exec 70〜300 ms、スケジュール遅れ最大 5 ms。7 台ほぼ揃って変わる |
| unicast を 7 台に順送り（流す演出） | 全部 3 秒 timeout。直後の単発 unicast も 2.3 s。**不採用** |
| 見た目 | 100% で全色を回す → 虹色で クラブ感なし。**毎拍 100%→8% のパルス + 4 拍ごとに色替え**（暗い寒色系）が良い |

mando には長寿命で動き続ける仕組みが無い（唯一の長寿命子は `mat listen`）。
本体以外（シェルスクリプト等）のコードは増やしたくない（利用者の要望）ので、
**ループは mando の Rust 側で回し、拍ごとに exec するコマンドは config の配列**で与える。

## 設計原則との整合

- 原則 1・2（プロトコルを喋らない・バックエンド非依存）: mando が知るのは「config のコマンド配列を
  タイミング付きで exec する」ことだけ。照明の群・色・パルスの形は config。
- 原則 5（レーン直列化・timeout）: 各拍のコマンドは既存の `Executor::run` で走る（timeout 有界）。
  mat は matd が並行を捌くのでレーンは不要（デバイス名レーン = ループ自身の直列化のみ）。
- 原則 6・7（状態は pull・成否を正直に）: loop の状態はループタスクの in-memory 状態で、exec しない。
  各拍の exec 失敗はループを止めず、UI に 1 拍の失敗を見せもしない（音楽演出で 1 拍落ちは許容）。
  連続失敗は warn ログに出す。

## config

```toml
[[device]]
name  = "club_living"
alias = "クラブ（リビング）"
kind  = "loop"
bpms  = [100, 120, 128]   # UI のプリセットボタン。先頭が既定
max_minutes = 60          # 自動停止（省略時 60）
bar_beats = 4             # 1 小節の拍数（省略時 4）
# 毎拍、順に exec。{fade} は拍長の 80% を 0.1 秒単位に丸めた整数
beat = [
  ["mat", "group", "level", "--group", "living_lights", "--percent", "100"],
  ["mat", "group", "level", "--group", "living_lights", "--percent", "8", "--transition", "{fade}"],
]
# 小節の頭に beat より先に exec。小節ごとに順繰り（4 個なら 4 小節で一周）
bar = [
  ["mat", "group", "color", "--group", "living_lights", "--name", "magenta"],
  ["mat", "group", "color", "--group", "living_lights", "--name", "blue"],
  ["mat", "group", "color", "--group", "living_lights", "--name", "purple"],
  ["mat", "group", "color", "--group", "living_lights", "--name", "cyan"],
]
# 停止・自動停止・mando 終了時に順に exec（元の明かりに戻す）。
# shutter の `stop`（1 コマンド）と混ざらないよう別名にする
on_stop = [
  ["mat", "group", "color-temp", "--group", "living_lights", "--mireds", "370"],
  ["mat", "group", "level", "--group", "living_lights", "--percent", "100"],
]
```

検証（`config.rs`、既存の `require` / `forbid` と同じ流儀）:

- `kind = "loop"` は `beat`（1 個以上）と `on_stop`（1 個以上）必須。`bar` は任意（空なら小節処理なし）。
- `bpms` は 1 個以上、各値 40〜200。`max_minutes` は 1〜600。`bar_beats` は 1〜16。
- `beat` / `bar` / `on_stop` の各コマンドは空配列不可。`{fade}` は `beat` のみで使える（`bar` / `on_stop` に
  あればエラー）。他のプレースホルダ（`{color}` 等）は loop では不可。
- `get_state` / `open` / `close` / `stop` / `on` / `off` / `color` / `brightness` / `preset` / `stateless` /
  `face` / `members` / `node_id` は loop で指定不可（`ForbiddenField`）。逆に `bpms` / `max_minutes` /
  `bar_beats` / `beat` / `bar` / `on_stop` は loop 以外で指定不可。

## mando 本体

### 状態

`App` に `loops: Mutex<HashMap<String, LoopRun>>` を持つ。

```rust
struct LoopRun {
    bpm: u32,
    started_at: Instant,
    deadline: Instant,          // started_at + max_minutes
    task: tokio::task::JoinHandle<()>,
    cancel: tokio::sync::watch::Sender<bool>,  // true で停止要求
}
```

### API

- `GET /api/devices/{name}/state`（既存ルート、loop では exec しない）
  → `{ "state": "running" | "stopped", "bpm": 120, "remaining_s": 3480 }`（stopped なら `bpm` / `remaining_s` は無し）。
- `POST /api/devices/{name}/start` body `{ "bpm": 123 }`（任意。省略時 `bpms[0]`。整数 40〜200 以外は 400
  `invalid_bpm`）。すでに running なら**その場で止めて**（`on_stop` は実行しない — 直後に上書きされる）新しい
  bpm で起動する。応答は start 後の state と同じ形。
- `POST /api/devices/{name}/stop`（既存ルートを kind で分岐）: ループを止め、`on_stop` を順に exec。
  stopped のときに呼ばれても `on_stop` は exec する（照明を戻す用途で冪等に押せる）。応答は state と同じ形。
- `GET /api/devices` の一覧に loop も載せ、`kind: "loop"`, `bpms`, `max_minutes` を含める（UI がボタンを作る）。

### ループタスク

- 拍長 `beat_ms = 60_000 / bpm`。`fade = round(beat_ms * 0.8 / 100)`（0.1 秒単位・最小 1）。
- 開始時刻からの**絶対スケジュール**で `tokio::time::sleep_until(start + i * beat)`。
  拍 `i` の処理: `i % bar_beats == 0` なら `bar[(i / bar_beats) % bar.len()]` を exec → `beat` を順に exec。
  同じ拍のコマンドは**直列**（順序が意味を持つ: 色 → 100% → フェード）。
- 処理が次の拍の時刻を越えていたら、**越えた分の拍は飛ばして**次の未来の拍に揃える（遅れを積まない）。
- exec は `Executor::run(device.exec_lane(), cmd)`（既存の timeout 有界）。失敗（non-zero / timeout）は
  debug ログ、**連続 10 拍失敗で warn 1 回**（成功で復帰したらまた 10 回からカウント）。ループは止めない。
- `cancel` が true になるか `deadline` に達したら終了。deadline 到達時は `on_stop` を exec して
  `loops` から自分を外す（自動停止）。
- `on_stop` の exec は stop ハンドラ / deadline 到達 / shutdown のいずれも同じ関数。
- **shutdown**: `run_shutdown` で全 loop に cancel → `on_stop` を exec（各 exec は timeout 有界なので
  shutdown は有界）→ 既存の push 停止・axum drain へ。`stop_grace_period`（hogar は 10 s）に収めるため、
  shutdown 時の `on_stop` は全 loop 分まとめて **5 秒**で打ち切る。

### 変更ファイル

- `src/config.rs`: `Kind::Loop`、`Device` に `bpms` / `max_minutes` / `bar_beats` / `beat` / `bar` / `on_stop`、
  検証、`config.example.toml` に例。
- `src/main.rs`: `loops` 状態、`start` ルート、`stop` / `state` / `list_devices` の loop 分岐、ループタスク、
  shutdown 連携。ループ本体は `src/looper.rs` に分ける（`push.rs` と同じ粒度）。
- `index.html`: loop カード。

## UI（`index.html`）

- 操作タブに loop ごとにカード 1 枚（見出しは `alias`）。
- 表示: 停止中は「停止中」。再生中は「120 BPM で再生中 ・ 残り 58 分」。state は再生中のみ 5 秒ごとに取得
  （exec しないので軽い。停止中はポーリングしない）。
- ボタン: `bpms` の数だけプリセット（押すと即 start）、`タップ`、`停止`。
- タップ: 叩いた時刻を記録し、直近 8 回までの**間隔の中央値**から BPM を算出してタップボタンに表示。
  最後のタップから **1.5 秒**叩かれなければその BPM で start（再生中なら合わせ直し）。
  間隔が 2 秒を超えたら叩き直しとして記録をリセット。2 回未満なら何もしない。算出値は 40〜200 に丸める。
- POST の応答（state）で表示を更新する。確認読みは不要。
- 再生中に `リビング照明` カードの色・明るさを変えても loop が上書きし続ける。UI では縛らない。

## エラー処理

| 状況 | 挙動 |
|---|---|
| `start` の bpm 不正 | 400 `{ "error": "invalid_bpm" }` |
| loop 以外の device に `start` | 404（既存の not_found） |
| 拍の exec 失敗 | ループ継続。連続 10 回で warn 1 回 |
| `on_stop` の exec 失敗 | warn。state は stopped にする（照明が戻らなければ利用者が `リビング照明` の電球色を押す） |
| mando 再起動 | ループは消える（in-memory）。起動時に `on_stop` は走らせない（消灯中に点けてしまう） |

## テスト

- `config.rs`: loop の必須 / 禁止フィールド、`bpms` / `max_minutes` / `bar_beats` の値域、`{fade}` の位置制約、
  `on_stop` 必須、既存 kind に `beat` 等があれば `ForbiddenField`。
- `looper.rs`: ダミーコマンド（`sh -c 'echo ... >> file'` 等、既存テストの流儀）で
  - 拍ごとに `beat` が順に、小節頭に `bar` が順繰りに exec される（高 BPM・短い `max` で数拍だけ回す）
  - `{fade}` が bpm から計算した値に置換される
  - cancel で止まり `on_stop` が exec される
  - deadline で自動停止し `on_stop` が exec される
  - 遅い拍（`sleep` を挟む）は飛ばされ、遅れが積まれない
- `main.rs`（ルーター経由）: `start` → `state` running → `stop` → `state` stopped、再 `start` で bpm が
  変わり古いタスクが終わる、bpm 不正は 400、shutdown で `on_stop` が走る。
- 実機: 配備後に 120 BPM で 20 秒、2026-10-05 のテストと同じ見た目になるか目視。

## 配備

mando をビルド → hogar-iac の pin 更新・`hogar/config/mando/config.toml` に loop デバイス追加 →
despliegue skill の「NAS Docker（hogar）向け」手順で反映。
