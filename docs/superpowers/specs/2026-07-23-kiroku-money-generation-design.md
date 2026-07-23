# きろく: 電力収支「円（儲け）」・太陽光「定格比%」表示

- 対応 issue: [nogu3/mando#3](https://github.com/nogu3/mando/issues/3)（下層の対 issue: nogu3/embalse#13）
- 日付: 2026-07-23

## 目的

「きろく」で電力収支を kWh、太陽光発電を W のまま出しており、家族が見て
「儲かったの? いまの発電は良い/悪い?」が読めない。

- **電力収支** → 買電/売電/自家消費節約を **円** で内訳（スタックド棒）表示。見出しは
  「今日いくら得した/損した」のネット円。
- **太陽光発電** → **定格比%**（定格に対して何%で発電中）を主役に、今日合計 kWh を併記。

金額換算・定格比換算・単価・定格容量は **embalse（下層）の責務**（設計原則 2/4）。
mando は **表示のみ**。

## 契約（embalse と合意・確定）

行形式は現行どおりフラット配列 `[{ts, series?, value}]`。これに以下を足す:

- **予約センチネル `series == "@summary"`**（綴り確定）。この行は「sub 要約値」。
  通常系列から**除外**し、見出し併記の 1 値として使う。単位は graph config の `unit_daily`。
  - `@summary` が複数行来た場合は **ts 最大（最新）の value** を採用
    （現契約では generation today の単一行のみ。多重は保険）。
- `power_balance`: series = 買電(負) / 売電(正) / 自家消費節約(正)、value = 円。
  today = 1 日分の 3 行、week/month = 日別 3 行 × N。`consumption` は円系列に含めない。
  買電が負符号で来るので、mando は符号を知らずに積み上げ＆ネット合算できる
  （ネット = Σ = 売電 + 節約 − 買電）。料金の符号知識は embalse に閉じる。
- `generation` today: value = %、`@summary` 行 = 今日合計 kWh。

**下層固有知識（センチネル綴り）は `normalize.rs` の一点に閉じる**（設計原則 4）。

## 変更点（5 ファイル）

### 1. `config.rs`

- `Graph` に `chart: Option<String>` を追加（`#[serde(default)]`、省略 = 従来 line/bar 挙動）。
- 検証を既存様式（重複 / 空 query / `{period}` 個数）に合わせて追加:
  **既知値 `"stacked"` のみ許可**。未知値は `ConfigError::UnknownChart { graph, value }` で弾く。

### 2. `normalize.rs`

- `normalize_graph_rows` の戻りを **`struct GraphNormalized { series: Vec<GraphSeries>, summary: Option<f64> }`**
  に変更（タプルでなく名前付き — 呼び出し側の可読性）。
- `series == "@summary"` の行を series 束ねから**除外**し、`summary` へ回す
  （複数あれば ts 最大の value）。
- 既存テストは戻り値の型変更に追従。

### 3. `main.rs`

- `GraphView` に以下を追加（いずれも `Option`・`None` は JSON 省略）:
  - **`chart: Option<String>`**（config 由来）
  - **`summary: Option<f64>`**（正規化結果）
  - **`summary_unit: Option<String>`** — summary の表示単位。`GraphView.unit` は period
    解決済み（generation today では `%`）なので、kWh の summary を出すには別枠が要る。
    summary が `Some` のとき `graph.unit_daily`（無ければ `graph.unit`）を載せる。
- `get_graph` は正規化結果の `series` / `summary`、config の `chart` / summary_unit を載せるだけ。

### 4. `index.html`

- **`view.name === "generation"` の特別扱いと W 積分 `Σv×(5/60)÷1000` を撤去**
  （設計原則 4: generation 固有名をコードから排除。合計は embalse が `@summary` で返す）。
- 見出しロジックを 2 モードに:
  - **通常**（`chart` 未指定）: 見出し = 先頭系列の最新値 + `unit`。`summary` があれば
    sub に「今日 {summary} {summary_unit}」。→ 発電は「いま X% / 今日 Y kWh」がこれで出る。
  - **`chart === "stacked"`**: 見出し = **ネット合計 Σ**（全系列全点の合算。買電が負なので
    単純合算がネット）+ `unit`。得 = プラス色 / 損 = マイナス色。本体 = **スタックド棒**。
- **セグメント配色は series ラベルでなく符号で決める**（設計原則 3/4: 買電/売電等の
  下層ラベルを UI コードに焼かない）。系列合計が負 → 支出色（グレー）、正 → 得色パレット
  （緑・青）を初出順に割当。ネット見出しの符号も全点 Σ の符号で決める。
- **スタックド棒描画関数を新規追加**（既存 line/bar は据え置き。dataviz スキルに従う）:
  - today = 1 本、week/month = 日別 N 本。
  - **基線 0** を挟み、買電（負）を**下**、売電・自家消費節約（正）を**上**に積む。
  - 正側の積み重ねはセグメント間 2px 地間隔。上端/下端の外側角のみ 4px 丸め。

#### 配色（案 E で確定）

| 用途 | 色 | 備考 |
|---|---|---|
| 買電（支出・下） | `#6b7488`（スレートグレー） | 赤で煽らずニュートラルに |
| 売電（収入・上） | `#18b88a`（`--open`） | |
| 自家消費節約（上） | `#6ea8fe`（`--accent`） | |
| ネット見出し 得(+) | `#18b88a`（`--gain`） | |
| ネット見出し 損(−) | `#f0635a`（`--loss` / `--warn`） | |

**dataviz 検証で許容した意図的逸脱（記録）:**
- 実際に隣接する 売電(緑)↔自家消費(青) の色弱識別 ΔE 16.8 / 通常視 20.3・コントラスト
  全色 ≥3:1 でいずれも PASS。読みやすさは担保。
- Chroma floor FAIL（買電グレー）は「支出をニュートラルに」という意図そのもの。
  基線下の位置 + 凡例で識別は色のみに依存しない。
- Lightness band FAIL（緑/青）は mando 既存 design token でアプリ統一のため。コントラスト PASS。

### 5. `config.example.toml`

- `power_balance`（`unit = "円"`, `chart = "stacked"`）と
  `generation`（`unit = "%"`, `unit_daily = "kWh"`）の例を追記。

## テスト（既存スタイル踏襲・TDD）

- **normalize**: `@summary` 抽出 / 通常系列からの除外 / 多重時の最新採用 /
  従来グラフ（`@summary` なし）が壊れないこと（`summary == None`）。
- **config**: `chart` の既知値 OK・未知値エラー・省略時の後方互換。
- **API ハンドラ**: `sh -c 'printf ...'` スタブ exec（円 3 系列・% ＋ `@summary` の固定 JSON）で
  `GraphView.chart` / `summary` / `series` を検証。
- **UI**（スタックド棒・ネット見出し・発電%）は手動 + headless-chromium スクショ確認。

## やらないこと

- キャッシュ / リアルタイム更新。
- mando 側での金額・定格計算（embalse に集約。設計原則 2/4）。
- 他メトリクス（CO2・温湿度・machine）への波及。
- 時間帯別・季節別の料金プラン（embalse 側でも当面 単価一定）。

## 実装順序

出力契約は本 spec（＋ embalse#13）で固定済み。mando はスタブ（固定 JSON の
`sh -c` / cat）でテスト・UI まで**先行実装可能**。embalse 後続。最後に jarvis/NAS で結合確認。
