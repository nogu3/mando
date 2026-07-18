# jarvis マシン情報の表示（machine グラフ + health バナー）設計

日付: 2026-07-18
状態: 承認済み（ブレスト完了）

## 目的

embalse 側で jarvis のマシンメトリクス（CPU・メモリ・ディスク・SoC 温度）が
取れるようになった（embalse リポ `2026-07-18-machine-health-design.md`）。
mando の画面から推移を見られるようにし、しきい値超え・収集停止に気づけるようにする。

- スコープ: machine グラフ（きろく）+ health バナー（異常時のみ表示）の両方
- embalse 側は配備済み前提。mando は 2 つの口を叩くだけ:
  - `embalse-query machine <period>` — 既存グラフ契約と同形の 4 系列
    （series = `cpu_used_pct` / `mem_used_pct` / `disk_used_pct` / `cpu_temp_c`）
  - `embalse-query health` — 直近 15 分の最新値をしきい値判定した
    `[{"metric", "value", "ts", "level"}]`（level = ok/warn/crit/stale、常に 4 要素）。
    しきい値判定は embalse の責務で、mando は判定しない

## 決定事項（ブレストでの選択）

- **health バナーは異常時のみ表示。** 全部 ok なら何も出さない。
  家族向けの操作画面を汚さず、異常には確実に気づける
- **series 表示名は config の汎用マップで解決**（案A）。metric 名 → 日本語の知識は
  config に留め、mando 本体は知らない（設計原則 2 と同型）。normalize.rs に
  embalse 固有名を書く案は汎用グラフ正規化の純度が下がるため不採用
- **health は専用 `[health]` セクション + `GET /api/health`**（案A）。
  グラフ機構への相乗りは契約が違い（level あり・時系列でない）分岐だらけになるため不採用

## config（config.example.toml にも追記）

```toml
# きろくに machine グラフを追加（既存 [[graph]] 機構。実 config 側の変更）
[[graph]]
name  = "machine"
label = "jarvis"
unit  = "%"                       # 温度だけラベル側で ℃ を明示
query = ["embalse-query", "machine", "{period}"]
[graph.series_labels]             # ★新設（汎用・省略可）
cpu_used_pct  = "CPU (%)"
mem_used_pct  = "メモリ (%)"
disk_used_pct = "ディスク (%)"
cpu_temp_c    = "温度 (℃)"

# health（★新設セクション。未設定なら機能ごと無効）
[health]
command = ["embalse-query", "health"]
[health.labels]                   # metric 名 → 表示名（省略時は metric 名素通し）
cpu_used_pct  = "CPU"
mem_used_pct  = "メモリ"
disk_used_pct = "ディスク"
cpu_temp_c    = "CPU温度"
```

検証:

- `[health]` があれば `command` 非空を要求
- `series_labels` / `labels` は自由なマップで検証なし（知らないキーは単に使われない）

## API

`GET /api/health` — health テンプレを exec →正規化して返す:

```json
{
  "worst": "warn",
  "items": [
    {"label": "ディスク", "value": 83.2, "ts": "2026-07-18T10:05:00+09:00", "level": "warn"},
    {"label": "メモリ", "level": "stale"}
  ]
}
```

- exec は**グラフ用 Executor に相乗り**。3610 と無関係な読み系なので devices の
  semaphore には入れない。新規 Executor は作らない
- `[health]` 未設定時は 404。exec 失敗 / 契約外 JSON は 502 + `{"error"}`（グラフと同じ写像）
- `worst` は `crit > stale > warn > ok` の順で最悪値を取る。stale は「収集が止まっている」
  異常なので warn より上に置く（crit ほどの緊急ではないが気づくべき状態）

## 正規化（normalize.rs）

- `normalize_graph_rows` に series_labels マップ（`Option<&HashMap>`）を渡し、
  series 名を置換してからグループ化。マップに無い series は素通し。
  既存呼び出しは `None` 相当で挙動不変
- 新設 `normalize_health_rows`: 契約 `[{metric, value?, ts?, level}]` → 上記 items へ
  - `level` が 4 値（ok/warn/crit/stale）以外の行は drop
    （正直原則: 解釈できないものを ok と偽らない）
  - items が空（契約 0 行 / 全行 drop）なら `worst` は `"stale"`
    （判定材料ゼロ＝収集停止と同じ扱い）

## UI（index.html）

- **バナー**: `<header>` 直下に警告バナー領域を常設（普段は非表示）。
  `boot()` で `/api/health` を fetch し、`worst != "ok"` のときだけ表示
  - 例: `⚠ jarvis: ディスク 83% (注意)`。crit は赤系・warn は黄系・stale は灰系「収集停止」
  - 異常 items のみ列挙。タップで閉じられる（その表示中のみ。次回ロードで再判定）
  - fetch 失敗・404（機能無効）時は黙って何も出さない。監視の失敗で操作画面を
    壊さない（監視の失敗は操作の失敗ではない）。console.error には残す
- **ポーリングしない**。ページ表示（boot）時の 1 回のみ
  （マシン状態は 5 分粒度でしか動かない。原則 6 の趣旨に整合）
- **machine グラフ**: 既存きろく機構がそのまま描く。コード変更は series_labels の適用のみ

## テスト（既存スタイル踏襲）

- normalize: series_labels 置換 / マップ外素通し / health 正常・stale・
  不正 level drop・空→stale・worst 順位
- config: health command 空拒否 / health 未設定でも起動可
- API: `sh -c 'printf ...'` スタブで /api/health 200・502・未設定 404

## デプロイ

実 config（jarvis）に `[[graph]] machine` と `[health]` を追記 →
既存手順（deploy-incoming push → ff → build → install）。embalse 側は配備済み前提。

## やらないこと

- health のポーリング / プッシュ通知（embalse spec どおり画面表示のみ）
- mando 側でのしきい値判定（embalse の責務）
- 温度・% の軸分離などグラフの多軸化（1 グラフ同居で開始）
