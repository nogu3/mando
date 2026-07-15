# embalse データのグラフ表示（きろくセクション）設計

日付: 2026-07-15
状態: 承認済み（ブレスト完了）

## 目的

embalse（家庭内データレイク、jarvis の `/var/lib/embalse/raw/YYYY/MM/DD/metrics.jsonl`）に
蓄積中のセンサーデータを、mando の操作画面から家族が見られるようにする。
「今日どれくらい発電した？」「書斎の CO2 はいま高い？」に一目で答えるのが目標。

- 利用者: 家族（操作画面の一部として表示）
- 期間切替: 今日 / 週 / 月
- 初期メトリクス: 太陽光発電、電力消費（買電/売電含む）、CO2（部屋別）、温度・湿度（部屋別）

## アーキテクチャ

採用案: **B — embalse 側にクエリ CLI を置き、mando は config テンプレで exec する。**
（A: config に SQL 直書き → config 肥大化で不採用。C: mando が JSONL 直読み →
バックエンド非依存原則違反で不採用。）

```
[embalse 側 (jarvis)]
  embalse-query <graph> <period>     ← 新設 CLI（duckdb/queries/*.sql を duckdb -json で実行）
    ↓ stdout に契約 JSON 配列
[mando 側]
  config.toml の [[graph]] テンプレ   ← ["embalse-query", "generation", "{period}"]
    ↓ exec（グラフ専用 Executor。devices の Executor とは別インスタンス）
  正規化関数（graph 版, normalize.rs） ← 契約 JSON → チャート系列。下層固有知識はここ一点
    ↓
  GET /api/graphs / GET /api/graphs/{name}?period=...
    ↓
  index.html「きろく」セクション      ← SVG 手描きチャート（外部ライブラリなし、焼き込み維持）
```

責務の線引き:

- SQL・スキーマ・JSONL パス・集計ロジック（energy 累積の日次 max、時間ビニング、JST 境界）
  はすべて **embalse**。
- mando は「exec して契約 JSON をチャート系列へ正規化して配る」だけ。
  `embalse-query` という名前すら本体は知らない（config が知っている）。

### Executor の分離

グラフ用に**別の `Executor`（専用 Semaphore(1)）** を持つ。既存の semaphore は
3610 ポート衝突対策であり、duckdb 読みは 3610 と無関係。同じ枠に入れると
月間クエリ（数秒かかりうる）がシャッター操作をブロックするため分離する。
グラフ同士は直列（raspi の CPU/メモリ保護）。

## CLI 出力契約（embalse 側実装の仕様を兼ねる）

`embalse-query <graph> <period>` は stdout に次の JSON 配列を出力し、成功時は
終了コード 0 を返す。

```json
[
  {"ts": "2026-07-15T00:00:00+09:00", "series": "発電", "value": 1234.0},
  {"ts": "2026-07-15T00:05:00+09:00", "series": "発電", "value": 1300.0}
]
```

| field  | type   | 必須 | 説明 |
|--------|--------|------|------|
| ts     | string | ◯   | ISO8601。今日/週/月の境界は **JST**（embalse の保存は UTC。DuckDB 側で `SET TimeZone='Asia/Tokyo'` して切る。embalse の責務） |
| series | string | △   | 系列ラベル。単系列グラフでは省略可。部屋別なら「書斎」「リビング」等 |
| value  | float  | ◯   | 観測値・集計値 |

`<period>` は `today` / `week` / `month` の 3 値。

### 初期グラフセットとクエリ仕様

| graph name | today | week / month |
|---|---|---|
| generation | `power_generation_w` 5 分ビン平均カーブ | `energy_generation_kwh` の日次 max（当日 0 時からの累積なので max = 日合計） |
| power_balance | 消費 / 買電 / 売電の 3 系列カーブ | `energy_{consumption,buy,sell}_kwh` 日次 max |
| co2 | `co2_ppm` 部屋別カーブ | 日別平均 |
| temperature | `temperature_c` 部屋別カーブ | 日別平均 |
| humidity | `humidity_pct` 部屋別カーブ | 日別平均 |

SQL 本体・テストは embalse リポジトリの実装計画で扱う（roadmap トラック B-1 と合流）。

## mando の config

```toml
[[graph]]
name       = "generation"          # API のパスセグメント（一意）
label      = "太陽光発電"           # UI 表示名
unit       = "W"                   # 今日ビュー時の単位
unit_daily = "kWh"                 # 週/月（日別集計）ビュー時の単位。省略時 unit を使う
query      = ["embalse-query", "generation", "{period}"]
```

検証（既存パターン踏襲）:

- `name` 重複拒否
- `query` 空拒否
- `{period}` プレースホルダがちょうど 1 個

`[[graph]]` が 0 個なら UI にグラフセクション自体を出さない
（既存機能への影響ゼロ。graph 未設定でも従来どおり起動できる）。

## mando の API

| エンドポイント | 返すもの |
|---|---|
| `GET /api/graphs` | `[{"name":"generation","label":"太陽光発電"}, ...]` |
| `GET /api/graphs/{name}?period=today\|week\|month` | 下記 |

```json
{
  "name": "generation",
  "period": "today",
  "unit": "W",
  "series": [
    {"label": "発電", "points": [["2026-07-15T00:00:00+09:00", 0.0], ["...", 1234.0]]}
  ]
}
```

- `period` は enum 検証してから `{period}` に置換（任意文字列を subprocess に渡さない）。不正値は 400
- `unit` は period に応じて `unit` / `unit_daily` を選ぶ
- 正規化: 契約 JSON（フラット行配列）→ `series` 別グループ化 → `ts` 昇順ソート。
  `value` が数値でない行は drop。series 省略行は series をグラフの `label` と見なして束ねる

## UI（「きろく」セクション）

操作画面（index.html）の**最下部**に追加。デバイス操作が主役という第一目標を崩さない。

- 期間タブ（今日/週/月）は全グラフ共通で 1 つ
- 各グラフの見出しに**要約数値**: 今日ビュー = 最新値（generation のみ「今日の合計 kWh」併記。
  合計は UI 側で 5 分ビンの W 値を積分近似 `Σ value × (5/60) ÷ 1000` して算出する近似値）。
  数字が主、カーブは従
- チャートは **SVG 手描き**（外部ライブラリなし、`include_str!` 焼き込み・オフライン動作維持）。
  今日 = 折れ線、週/月 = 棒。実装時は dataviz スキルに従う
- 取得タイミング: セクション初回可視時（IntersectionObserver）+ 期間タブ切替時。
  **ポーリングしない**（データは 1〜5 分間隔でしか増えない）
- グラフごとに個別のローディング / エラー表示（1 枚失敗しても他は出す）
- キャッシュは v1 では入れない。グラフ Executor が直列なので同時アクセスは自己制限される。
  重くなったら name+period 単位の短 TTL キャッシュを後付け（Phase 2 のキャッシュ構想と同形）

## エラー処理

| 事象 | mando の応答 | UI 表示 |
|---|---|---|
| period が不正値 | 400 | （UI からは発生しない） |
| graph name が config に無い | 404 | 同上 |
| exec 非ゼロ終了 / spawn 失敗 | 502 + `{"error"}` | 「データを取得できませんでした」 |
| stdout が契約 JSON でない | 502 + `{"error"}` | 同上 |
| 契約 JSON だが 0 行 | 200 + 空 series | 「まだデータがありません」 |
| 一部の行が不正（value 非数値等） | その行だけ drop して 200 | 正常描画 |

欠けたデータをゼロ埋めや補間で誤魔化さない（設計原則 7「成否を正直に出す」のグラフ版）。

## テスト

mando 側（既存スタイル踏襲）:

- 正規化関数の単体テスト: 正常 / 複数系列 / 空 / 不正行 drop / 順序ソート
- config 検証テスト: name 重複・`{period}` 個数
- API ハンドラテスト: `sh -c 'printf ...'` でスタブ exec する既存パターン

embalse 側（別リポジトリの実装計画で扱う）:

- サンプル JSONL に対する各 SQL の期待値テスト（JST 境界・energy 日次 max）

## 実装順序（2 リポジトリ跨ぎ）

1. **mando 先行**: 出力契約は本設計で固定済み。契約 JSON を返すスタブ
   （`sh -c` / 固定 JSON の cat）でテスト・UI まで完成できる
2. **embalse 後続**: `embalse-query` CLI + `duckdb/queries/*.sql` を embalse 側の
   設計・計画として別途起こす
3. jarvis 上で実 config を繋いで結合確認

## やらないこと

- ポーリング / リアルタイム更新（データ間隔 1〜5 分に対して過剰）
- チャートライブラリの導入（焼き込み単一バイナリ・オフライン動作を崩す）
- mando 本体への embalse スキーマ・パスの焼き込み（設計原則 2 違反）
- v1 でのキャッシュ・水道/ガスグラフ（必要になってから）
