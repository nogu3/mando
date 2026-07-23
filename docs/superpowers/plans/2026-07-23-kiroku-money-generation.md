# きろく 円・定格比% 表示 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** きろくの電力収支を円のスタックド棒（ネット見出し）に、太陽光を定格比%＋今日合計 kWh に変える。

**Architecture:** 下層 embalse が円・%・`@summary`（今日合計）を返す契約に対し、mando は表示だけを担う。`chart="stacked"` を config に足し、`@summary` センチネルを `normalize.rs` の一点で拾い、UI に 2 つの見出しモード（通常 / stacked）と新しいスタックド棒描画を足す。

**Tech Stack:** Rust（axum / serde / toml / serde_json）、焼き込み `index.html`（外部ライブラリなしの手描き SVG）。

## Global Constraints

- **プロトコルを直接喋らない**。exec は config テンプレのまま（設計原則 1/2）。
- **下層固有知識は `normalize.rs` の一点に閉じる**（設計原則 4）。センチネル綴りは `@summary`（確定）。
- **series ラベル（買電/売電等）を UI・API コードに焼かない**（設計原則 3/4）。stacked の色は series 合計の**符号**で決める。
- 契約行形式: フラット配列 `[{ts, series?, value}]`。`series == "@summary"` は sub 要約値（通常系列から除外・複数なら ts 最大を採用）。
- `power_balance`: 買電(負)/売電(正)/自家消費節約(正)、value=円。`generation` today: value=%、`@summary`=今日合計 kWh。
- 配色（案 E 確定）: 買電=`#6b7488` / 売電=`#18b88a` / 自家消費節約=`#6ea8fe` / ネット得(+)=`#18b88a` / ネット損(−)=`#f0635a`。
- 既存テストの書式踏襲: config は `write_tmp`＋`Config::load`＋`matches!`、API は `sh -c` printf スタブ＋`call()`、normalize は `json!` 行配列。
- 検証コマンド: `cargo test`、`cargo clippy -- -D warnings`。

---

### Task 1: config.rs — `Graph.chart` フィールドと検証

**Files:**
- Modify: `src/config.rs`（`struct Graph` 付近 135-153、`enum ConfigError` 292-317、Display 318-357、graph 検証ループ 599-614、テストモジュール末尾）

**Interfaces:**
- Produces: `Graph.chart: Option<String>`（既知値 `"stacked"` のみ許可、省略可）。`ConfigError::UnknownChart { graph: String, value: String }`。

- [ ] **Step 1: 未知 chart 値を弾く失敗テストを書く**

`src/config.rs` のテストモジュール（`fn loads_valid_config` の近く）に追加:

```rust
    #[test]
    fn graph_chart_stacked_accepted() {
        let p = write_tmp(
            "chart_ok",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl","get","x","026301","open_close_state"]
            open = ["enl","set","x","026301","open_close_operation","open"]
            close = ["enl","set","x","026301","open_close_operation","close"]
            [[graph]]
            name  = "power_balance"
            unit  = "円"
            chart = "stacked"
            query = ["curl","{period}"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find_graph("power_balance").unwrap().chart.as_deref(), Some("stacked"));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_chart_unknown_rejected() {
        let p = write_tmp(
            "chart_bad",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl","get","x","026301","open_close_state"]
            open = ["enl","set","x","026301","open_close_operation","open"]
            close = ["enl","set","x","026301","open_close_operation","close"]
            [[graph]]
            name  = "g"
            unit  = "円"
            chart = "pie"
            query = ["curl","{period}"]
            "##,
        );
        assert!(matches!(Config::load(&p), Err(ConfigError::UnknownChart { .. })));
        std::fs::remove_file(p).ok();
    }
```

- [ ] **Step 2: テストを走らせて失敗を確認**

Run: `cargo test --lib config::tests::graph_chart 2>&1 | tail -20`
Expected: コンパイルエラー（`chart` フィールド無し / `UnknownChart` 無し）。

- [ ] **Step 3: `Graph` に `chart` を追加**

`src/config.rs` の `struct Graph`（`series_labels` フィールドの直後、153 付近）に追加:

```rust
    /// チャート形（任意）。既知値 "stacked" のみ。省略時は従来 line/bar。
    #[serde(default)]
    pub chart: Option<String>,
```

- [ ] **Step 4: `ConfigError::UnknownChart` を追加**

`enum ConfigError`（`PeriodPlaceholder` の直後、310 付近）に:

```rust
    UnknownChart { graph: String, value: String },
```

Display（`ConfigError::PeriodPlaceholder { .. } => ...` の直後、358 付近）に:

```rust
            ConfigError::UnknownChart { graph, value } => {
                write!(f, "graph {graph}: 未知の chart 値 {value}（対応: stacked）")
            }
```

- [ ] **Step 5: graph 検証ループに chart チェックを足す**

`src/config.rs` の graph ループ（`{period}` 個数チェックの直後、613 付近、`}` で閉じる前）に:

```rust
            if let Some(c) = &g.chart {
                if c != "stacked" {
                    return Err(ConfigError::UnknownChart {
                        graph: g.name.clone(),
                        value: c.clone(),
                    });
                }
            }
```

- [ ] **Step 6: テストが通ることを確認**

Run: `cargo test --lib config::tests::graph_chart 2>&1 | tail -20`
Expected: `graph_chart_stacked_accepted` と `graph_chart_unknown_rejected` が PASS。

- [ ] **Step 7: コミット**

```bash
git add src/config.rs
git commit -m "feat(config): Graph に chart=stacked を追加（#3）"
```

---

### Task 2: normalize.rs — `GraphNormalized` と `@summary` 抽出

**Files:**
- Modify: `src/normalize.rs`（`normalize_graph_rows` 125-180、既存テスト 490-560）
- Modify: `src/main.rs`（call site 724-725、import 25）— シグネチャ変更でコンパイルを保つための最小追随

**Interfaces:**
- Consumes: `GraphSeries`（既存）。
- Produces: `pub struct GraphNormalized { pub series: Vec<GraphSeries>, pub summary: Option<f64> }`。`normalize_graph_rows(...) -> GraphNormalized`。`series == "@summary"` の行は series から除外し summary（ts 最大の value）へ。

- [ ] **Step 1: `@summary` 抽出の失敗テストを書く**

`src/normalize.rs` のテストモジュール（`graph_rows_empty_is_empty` の後、546 付近）に追加:

```rust
    #[test]
    fn graph_rows_summary_extracted_and_excluded() {
        let rows = [
            json!({"ts": "2026-07-15T10:00:00+09:00", "value": 100.0}),
            json!({"ts": "2026-07-15T10:05:00+09:00", "value": 200.0}),
            json!({"ts": "2026-07-15T23:59:00+09:00", "series": "@summary", "value": 5.6}),
        ];
        let n = normalize_graph_rows(&rows, "太陽光発電", None);
        assert_eq!(n.summary, Some(5.6));
        assert_eq!(n.series.len(), 1); // @summary は通常系列に混ざらない
        assert_eq!(n.series[0].points.len(), 2);
    }

    #[test]
    fn graph_rows_summary_takes_latest_ts() {
        let rows = [
            json!({"ts": "t1", "series": "@summary", "value": 1.0}),
            json!({"ts": "t3", "series": "@summary", "value": 3.0}),
            json!({"ts": "t2", "series": "@summary", "value": 2.0}),
        ];
        let n = normalize_graph_rows(&rows, "x", None);
        assert_eq!(n.summary, Some(3.0)); // ts 最大
        assert!(n.series.is_empty());
    }

    #[test]
    fn graph_rows_no_summary_is_none() {
        let rows = [json!({"ts": "t1", "value": 1.0})];
        let n = normalize_graph_rows(&rows, "x", None);
        assert_eq!(n.summary, None);
        assert_eq!(n.series.len(), 1);
    }
```

- [ ] **Step 2: 既存テストを新シグネチャに追随させる**

同ファイルの既存 graph テスト（491-560）で `normalize_graph_rows(...)` の戻りを使う箇所を `.series` 経由に直す。具体的に:
- `let s = normalize_graph_rows(...)` → `let s = normalize_graph_rows(...).series;`（`graph_rows_single_series_gets_default_label` / `_grouped_by_series...` / `_sorted_by_ts...` / `_invalid_rows_dropped` / `_series_labels_mapped`）
- `graph_rows_empty_is_empty`: `assert!(normalize_graph_rows(&[], "x", None).series.is_empty());`

- [ ] **Step 3: テストを走らせて失敗を確認**

Run: `cargo test --lib normalize 2>&1 | tail -20`
Expected: コンパイルエラー（`GraphNormalized` 無し / `.series` フィールド無し）。

- [ ] **Step 4: `GraphNormalized` を定義し戻り値を変更**

`src/normalize.rs` の `struct GraphSeries`（131 付近）の直後に:

```rust
/// グラフ正規化の結果。通常系列と、見出し併記用の sub 要約値（@summary 行）。
#[derive(Debug, PartialEq, Serialize)]
pub struct GraphNormalized {
    pub series: Vec<GraphSeries>,
    pub summary: Option<f64>,
}
```

`normalize_graph_rows` の戻り型を `-> GraphNormalized` に変更し、行ループと末尾を差し替える。行ループ冒頭（`ts` 取得の直後、`value` 取得の後）で `@summary` を先取りする:

```rust
pub fn normalize_graph_rows(
    rows: &[Value],
    default_label: &str,
    series_labels: Option<&std::collections::HashMap<String, String>>,
) -> GraphNormalized {
    let mut order: Vec<String> = Vec::new();
    let mut by_label: std::collections::HashMap<String, Vec<(String, f64)>> =
        std::collections::HashMap::new();
    let mut summary: Option<f64> = None;
    let mut summary_ts: Option<String> = None;
    for row in rows {
        let Some(ts) = row.get("ts").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = row.get("value").and_then(Value::as_f64) else {
            continue;
        };
        let series_field = row.get("series").and_then(Value::as_str);
        // 予約センチネル: 通常系列に混ぜず、ts 最大の value を sub 要約値に。
        if series_field == Some("@summary") {
            if summary_ts.as_deref().map_or(true, |t| ts > t) {
                summary = Some(value);
                summary_ts = Some(ts.to_string());
            }
            continue;
        }
        let raw = series_field.unwrap_or(default_label);
        let label = series_labels
            .and_then(|m| m.get(raw))
            .map(String::as_str)
            .unwrap_or(raw);
        if !by_label.contains_key(label) {
            order.push(label.to_string());
        }
        by_label
            .entry(label.to_string())
            .or_default()
            .push((ts.to_string(), value));
    }
    let series = order
        .into_iter()
        .map(|label| {
            let mut points = by_label.remove(&label).unwrap_or_default();
            points.sort_by(|a, b| a.0.cmp(&b.0));
            GraphSeries { label, points }
        })
        .collect();
    GraphNormalized { series, summary }
}
```

Doc コメント（133-140）に「`@summary` 行は sub 要約値として抽出し series から除外（ts 最大を採用）」の一文を足す。

- [ ] **Step 5: main.rs の call site をコンパイル可能に保つ（summary はこのタスクでは未使用）**

`src/main.rs:724-725` を差し替え:

```rust
    let series =
        normalize::normalize_graph_rows(&rows, graph.label(), graph.series_labels.as_ref()).series;
```

（`GraphView` への summary 配線は Task 3。ここは `.series` を取るだけで従来挙動を維持。）

- [ ] **Step 6: テストが通ることを確認**

Run: `cargo test --lib normalize 2>&1 | tail -20` then `cargo test 2>&1 | tail -15`
Expected: normalize の新旧テスト全 PASS、既存 main テストも PASS。

- [ ] **Step 7: コミット**

```bash
git add src/normalize.rs src/main.rs
git commit -m "feat(normalize): @summary センチネルを sub 要約値として抽出（#3）"
```

---

### Task 3: main.rs — `GraphView` に chart / summary / summary_unit を配線

**Files:**
- Modify: `src/main.rs`（`struct GraphView` 662-668、`get_graph` の `Json(GraphView{..})` 724-731、テスト用 `test_app` の graph スタブ 867-899、graph テスト群 1225-）

**Interfaces:**
- Consumes: `GraphNormalized`（Task 2）、`Graph.chart`（Task 1）。
- Produces: `GraphView { name, period, unit, chart: Option<String>, summary: Option<f64>, summary_unit: Option<String>, series }`。`None` フィールドは JSON 省略。

- [ ] **Step 1: stacked と @summary の API 失敗テストを書く**

まず `test_app`（`src/main.rs` 867-899）の graph 群に、既存 `generation` の query へ `@summary` 行を足し、`power_balance` スタックドスタブを追加する。

`generation` の query 行（872）を差し替え:

```rust
            query      = ["sh", "-c", "printf '[{\"ts\":\"2026-07-15T10:05:00+09:00\",\"value\":200},{\"ts\":\"2026-07-15T10:00:00+09:00\",\"value\":100},{\"ts\":\"2026-07-15T23:59:00+09:00\",\"series\":\"@summary\",\"value\":5.6}]'", "sh", "{period}"]
```

`machine` グラフ定義（894-899）の直後に追加:

```rust
            [[graph]]
            name  = "power_balance"
            unit  = "円"
            chart = "stacked"
            query = ["sh", "-c", "printf '[{\"ts\":\"t1\",\"series\":\"買電\",\"value\":-80},{\"ts\":\"t1\",\"series\":\"売電\",\"value\":180},{\"ts\":\"t1\",\"series\":\"自家消費節約\",\"value\":130}]'", "sh", "{period}"]
```

次にテスト群（`graph_today_normalized_and_sorted` の近く、1236 付近）に追加:

```rust
    #[tokio::test]
    async fn graph_summary_from_sentinel() {
        let (st, v) = call("GET", "/api/graphs/generation").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["summary"], 5.6);        // @summary 行が sub 要約値に
        assert_eq!(v["summary_unit"], "kWh"); // unit_daily
        // @summary は series に混ざらない（先頭系列は generation のまま）。
        assert_eq!(v["series"][0]["label"], "太陽光発電");
        assert_eq!(v["series"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn graph_stacked_carries_chart_and_series() {
        let (st, v) = call("GET", "/api/graphs/power_balance").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["chart"], "stacked");
        assert_eq!(v["unit"], "円");
        assert_eq!(v["series"].as_array().unwrap().len(), 3);
        assert!(v.get("summary").is_none() || v["summary"].is_null()); // @summary 無し
    }
```

- [ ] **Step 2: テストを走らせて失敗を確認**

Run: `cargo test graph_summary_from_sentinel graph_stacked_carries 2>&1 | tail -20`
Expected: FAIL（`summary`/`chart`/`summary_unit` が返らない・null）。

- [ ] **Step 3: `GraphView` にフィールド追加**

`src/main.rs:662-668` を差し替え:

```rust
#[derive(Serialize)]
struct GraphView {
    name: String,
    period: String,
    unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    chart: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_unit: Option<String>,
    series: Vec<GraphSeries>,
}
```

- [ ] **Step 4: `get_graph` で配線**

`src/main.rs` の call site（Task 2 で `.series` にした 724-725 と続く `Json(GraphView{..})` 726-731）を差し替え:

```rust
    let normalized =
        normalize::normalize_graph_rows(&rows, graph.label(), graph.series_labels.as_ref());
    let summary_unit = normalized
        .summary
        .map(|_| graph.unit_daily.as_deref().unwrap_or(&graph.unit).to_string());
    Json(GraphView {
        name: graph.name.clone(),
        period: period.to_string(),
        unit: graph.unit_for(period).to_string(),
        chart: graph.chart.clone(),
        summary: normalized.summary,
        summary_unit,
        series: normalized.series,
    })
    .into_response()
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test 2>&1 | tail -15`
Expected: 新テスト 2 本 PASS、既存 graph テスト（`graph_today_normalized_and_sorted` 等）も PASS。

- [ ] **Step 6: clippy を通す**

Run: `cargo clippy -- -D warnings 2>&1 | tail -15`
Expected: 警告なし。

- [ ] **Step 7: コミット**

```bash
git add src/main.rs
git commit -m "feat(api): GraphView に chart/summary/summary_unit を配線（#3）"
```

---

### Task 4: index.html — 見出し 2 モード・スタックド棒・generation 特別扱い撤去

**Files:**
- Modify: `index.html`（色定数 1037-1038、`renderGraph` 1116-1158、SVG チャート群 1160-1279、CSS のグラフ節 291 付近に見出し色/凡例スウォッチ形の追記）

**Interfaces:**
- Consumes: `GraphView` の `chart` / `summary` / `summary_unit` / `series`（Task 3）。
- Produces: `chart==="stacked"` のカードはネット見出し＋スタックド棒、それ以外は最新値＋（summary があれば）sub 併記。generation 固有分岐は消滅。

- [ ] **Step 1: 色定数を追加**

`index.html:1037` の `GRAPH_COLORS` 定義の下に追加:

```javascript
// スタックド棒（円収支）: 支出=ニュートラル、得=緑/青（案 E）。色は series の符号で割当。
const STACK_NEG_COLOR = "#6b7488";              // 支出（合計が負の系列）
const STACK_POS_COLORS = ["#18b88a", "#6ea8fe"]; // 得（合計が正の系列）を初出順に
const NET_GAIN = "#18b88a";                      // ネット見出し 得(+)
const NET_LOSS = "#f0635a";                      // ネット見出し 損(−)
```

- [ ] **Step 2: `renderGraph` を 2 モードに書き換える**

`index.html:1116-1158` の `renderGraph` 全体を差し替え:

```javascript
function renderGraph(gc, view) {
  const series = view.series.filter((s) => s.points.length).slice(0, GRAPH_COLORS.length);
  if (!series.length) {
    gc.bodyEl.innerHTML = `<div class="gmsg">まだデータがありません</div>`;
    return;
  }
  gc.valEl.style.color = ""; // 前回の得/損色をリセット
  const stacked = view.chart === "stacked";

  if (stacked) {
    // 見出し = 全系列全点のネット合計 Σ（買電が負なので単純合算がネット）。
    const net = series.reduce((a, s) => a + s.points.reduce((b, p) => b + p[1], 0), 0);
    const sign = net >= 0 ? "+" : "−";
    gc.valEl.textContent = sign + fmtNum(Math.abs(net));
    gc.valEl.style.color = net >= 0 ? NET_GAIN : NET_LOSS;
  } else {
    // 通常: 見出し = 先頭系列の最新値。
    const first = series[0];
    const latest = first.points[first.points.length - 1][1];
    gc.valEl.textContent = fmtNum(latest);
  }
  const u = document.createElement("span");
  u.className = "u";
  u.textContent = view.unit;
  gc.valEl.appendChild(u);

  // sub 要約値（@summary 由来）。単位は summary_unit。
  if (view.summary != null) {
    gc.subEl.textContent = `今日 ${fmtNum(view.summary)} ${view.summary_unit ?? ""}`.trim();
  }

  // stacked は series の符号で色を割当、それ以外は GRAPH_COLORS。
  const colorFor = stacked ? assignStackColors(series) : series.map((_, i) => GRAPH_COLORS[i]);

  // 凡例は 2 系列以上のときだけ（単系列はタイトルが系列名を兼ねる）。
  gc.legendEl.innerHTML = "";
  gc.legendEl.hidden = series.length < 2;
  if (series.length >= 2) {
    series.forEach((s, i) => {
      const k = document.createElement("span");
      k.className = "k";
      const d = document.createElement("span");
      d.className = "kd";
      d.style.background = colorFor[i];
      k.append(d, s.label);
      gc.legendEl.appendChild(k);
    });
  }

  gc.bodyEl.innerHTML = "";
  let svg;
  if (stacked) {
    svg = drawStackedChart(series, view.period, colorFor);
  } else {
    // 今日 = 時系列カーブ。週/月 = 日別集計（単系列は棒、複数系列は折れ線）。
    svg =
      view.period !== "today" && series.length === 1
        ? drawBarChart(series[0], view.period)
        : drawLineChart(series, view.period);
  }
  gc.bodyEl.appendChild(svg);
  attachReadout(svg, gc, series, view);
}

// series 合計の符号で色を割当（買電/売電等のラベルを UI に焼かない・設計原則 3/4）。
function assignStackColors(series) {
  let posIdx = 0;
  return series.map((s) => {
    const total = s.points.reduce((a, p) => a + p[1], 0);
    if (total < 0) return STACK_NEG_COLOR;
    const c = STACK_POS_COLORS[posIdx % STACK_POS_COLORS.length];
    posIdx += 1;
    return c;
  });
}
```

- [ ] **Step 3: `drawStackedChart` と `stackSeg` を追加**

`index.html` の `drawBarChart`（1252-1279）の直後に、両関数を追加する（JS の関数宣言は巻き上げされるので呼び出し順は問わない）:

```javascript
// スタックド棒: 基線 0 を挟み、正の系列を上、負の系列を下に積む。
// today=1 本、週/月=ts 別 N 本。色は colors[seriesIndex]。
function drawStackedChart(series, period, colors) {
  const { w, h, l, r, t, b } = PLOT;
  const svg = svgNode("svg", { viewBox: `0 0 ${w} ${h}`, role: "img" });
  // ts の和集合を昇順に（同一クエリ由来の ISO8601 は辞書順=時刻順）。
  const tsSet = new Set();
  series.forEach((s) => s.points.forEach((p) => tsSet.add(p[0])));
  const tss = [...tsSet].sort();
  const at = (s, ts) => {
    const p = s.points.find((q) => q[0] === ts);
    return p ? p[1] : 0;
  };
  // 上下それぞれの最大積み高を求めて軸を切りよく丸める。
  let maxUp = 0, maxDn = 0;
  for (const ts of tss) {
    let up = 0, dn = 0;
    series.forEach((s) => { const v = at(s, ts); if (v >= 0) up += v; else dn += -v; });
    maxUp = Math.max(maxUp, up); maxDn = Math.max(maxDn, dn);
  }
  const top = niceCeil(Math.max(maxUp, 1));
  const bot = niceCeil(Math.max(maxDn, 1));
  const y0 = t + (h - t - b) * (top / (top + bot)); // 基線 y
  const upH = y0 - t, dnH = (h - b) - y0;
  // 基線（やや強め）。
  svg.appendChild(svgNode("line", {
    x1: l, y1: y0, x2: w - r, y2: y0,
    stroke: "rgba(150,170,210,0.28)", "stroke-width": 1,
  }));
  const slot = (w - l - r) / tss.length;
  const bw = Math.max(6, Math.min(period === "today" ? 56 : 24, slot - 4));
  // x 端ラベル（最初と最後の ts）。
  const xEnd = [];
  tss.forEach((ts, i) => {
    const x = l + slot * i + (slot - bw) / 2;
    if (i === 0) xEnd.push([x + bw / 2, "start", fmtTs(new Date(Date.parse(ts)), period)]);
    if (i === tss.length - 1) xEnd.push([x + bw / 2, "end", fmtTs(new Date(Date.parse(ts)), period)]);
    let yUp = y0, yDn = y0;
    series.forEach((s, si) => {
      const v = at(s, ts);
      if (v > 0) {
        const hh = (v / top) * upH;
        const y = yUp - hh;
        stackSeg(svg, x, y, bw, hh, colors[si], "up");
        yUp = y - 2; // 2px 地間隔
      } else if (v < 0) {
        const hh = (-v / bot) * dnH;
        const gap = yDn === y0 ? 0 : 2;
        stackSeg(svg, x, yDn + gap, bw, hh, colors[si], "down");
        yDn = yDn + gap + hh;
      }
    });
  });
  for (const [x, anchor, text] of xEnd) {
    const tx = svgNode("text", {
      x, y: h - b + 13, "text-anchor": anchor,
      "font-size": 9, fill: "var(--muted)",
    });
    tx.textContent = text;
    svg.appendChild(tx);
  }
  return svg;
}

// 積みセグメント 1 個。dir="up" は上端角、"down" は下端角のみ 4px 丸め。
function stackSeg(svg, x, y, bw, hgt, fill, dir) {
  const rr = Math.min(4, bw / 2, hgt);
  let d;
  if (dir === "up") {
    d = `M${x},${y + hgt} L${x},${y + rr} Q${x},${y} ${x + rr},${y}` +
        ` L${x + bw - rr},${y} Q${x + bw},${y} ${x + bw},${y + rr} L${x + bw},${y + hgt} Z`;
  } else {
    const yb = y + hgt;
    d = `M${x},${y} L${x + bw},${y} L${x + bw},${yb - rr} Q${x + bw},${yb} ${x + bw - rr},${yb}` +
        ` L${x + rr},${yb} Q${x},${yb} ${x},${yb - rr} Z`;
  }
  svg.appendChild(svgNode("path", { d, fill }));
}
```

- [ ] **Step 4: `attachReadout` を stacked で無害化**

`attachReadout`（1283 付近）は index ベースで `series[0].points` を辿る前提。stacked（today=1 点）でも既存ロジックで破綻しないが、ネット見出しを上書きしないよう、stacked のときは読み取り層を張らない。`renderGraph` 内の `attachReadout(svg, gc, series, view);` を次に変更:

```javascript
  if (!stacked) attachReadout(svg, gc, series, view);
```

- [ ] **Step 5: ビルドして UI を目視・スクショ確認**

Run: `cargo build --release 2>&1 | tail -5`
Expected: ビルド成功（`index.html` は `include_str!` で焼き込み）。

headless-chromium でスクショ確認（`memory: headless-chromium-on-wsl2` の手順）。確認ポイント:
- `chart="stacked"` のカードで基線 0 を挟み、支出=グレーが下・売電=緑/自家消費=青が上に積まれる。
- 見出しが「+230 円」（緑）/ 損の日は「−80 円」（赤）。
- generation カードが「いま X% / 今日 Y kWh」を出し、旧「約 … kWh」積分表記が消えている。
- 週ビューで日別 N 本のスタックド棒。

（実データが無ければ Task 3 の `sh -c` スタブ config でローカル起動して確認。）

- [ ] **Step 6: コミット**

```bash
git add index.html
git commit -m "feat(ui): 円スタックド棒・ネット見出し・発電% と generation 特別扱い撤去（#3）"
```

---

### Task 5: config.example.toml — 円収支・発電% の例を追記

**Files:**
- Modify: `config.example.toml`（graph 節 189-211）

**Interfaces:**
- Consumes: `chart`（Task 1）、`unit` / `unit_daily`（既存）。

- [ ] **Step 1: 既存 `generation` 例を定格比%へ更新**

`config.example.toml:189-194` の `generation` 例を差し替え:

```toml
# [[graph]]
# name       = "generation"
# label      = "太陽光発電"
# unit       = "%"       # 今日ビュー: 定格比%（embalse が W/定格×100 で返す）
# unit_daily = "kWh"     # 週/月ビュー & 今日の @summary（今日合計 kWh）の単位
# query      = ["curl", "-fsS", "--max-time", "30", "http://192.0.2.20:8526/api/graphs/generation?period={period}"]
```

- [ ] **Step 2: `power_balance`（円スタックド）例を追記**

`generation` 例の直後に追加:

```toml
#
# [[graph]]
# name  = "power_balance"
# label = "電力収支"
# unit  = "円"           # embalse が 買電(負)/売電(正)/自家消費節約(正) を円で返す
# chart = "stacked"      # 基線 0 を挟んで積み上げ・見出しはネット合計（得/損）
# query = ["curl", "-fsS", "--max-time", "30", "http://192.0.2.20:8526/api/graphs/power_balance?period={period}"]
```

- [ ] **Step 3: コミット**

```bash
git add config.example.toml
git commit -m "docs(config): power_balance 円スタックド・発電% の例を追記（#3）"
```

---

## Self-Review

- **Spec coverage:** config `chart`（T1）/ normalize `@summary`（T2）/ API chart・summary・summary_unit（T3）/ UI 2 モード・スタックド棒・generation 撤去（T4）/ config.example（T5）— スペック全節に対応タスクあり。
- **Placeholder scan:** Task 4 Step 3 の仮 `stackSeg` は Step 4 で正式定義に置換する旨を明記済み（意図的な 2 段構成）。他にプレースホルダなし。
- **Type consistency:** `GraphNormalized { series, summary }`（T2）→ `get_graph` が `.series` / `.summary` 参照（T2 Step5 / T3 Step4）、`GraphView` の `chart/summary/summary_unit`（T3）→ UI が `view.chart/summary/summary_unit`（T4）で一致。`assignStackColors` / `drawStackedChart` / `stackSeg` の呼び出し名・引数順一致を確認済み。
