# embalse グラフ表示（きろくセクション）実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** embalse に蓄積中のセンサーデータ（太陽光発電・電力収支・CO2・温湿度）を、mando の操作画面最下部の「きろく」セクションで期間切替（今日/週/月）付きグラフとして表示する。

**Architecture:** config の `[[graph]]` テンプレ（`{period}` プレースホルダ）を専用 Executor で exec し、契約 JSON（`[{"ts","series","value"}]`）をチャート系列へ正規化して `GET /api/graphs/{name}?period=...` で返す。UI は焼き込み index.html 内の SVG 手描きチャート。スペック: `docs/superpowers/specs/2026-07-15-embalse-graphs-design.md`

**Tech Stack:** Rust (axum, serde, tokio) / vanilla JS + inline SVG（外部ライブラリなし）

## Global Constraints

- 外部 JS/CSS ライブラリ・CDN は追加しない（`include_str!` 焼き込み単一バイナリ・オフライン動作を維持）
- mando 本体に embalse のスキーマ・パス・SQL を持ち込まない（コマンド名は config だけが知る）
- グラフ exec は **devices とは別の** `Executor`（専用 Semaphore(1)）を使う
- `period` は `today` / `week` / `month` の 3 値のみ。検証前の文字列を subprocess に渡さない
- エラーマッピング（スペック確定値）: 不正 period=400 / 未知 graph=404 / exec 失敗・不正 JSON=502 / 0 行=200+空 series / 不正行=drop
- 系列色は検証済みパレット `["#3987e5", "#008300", "#d55181", "#c98500"]`（dataviz validator で mando ダーク面 `#10141d` に対し全チェック PASS 済み）。初出順に固定割当・循環させない
- Task 1・2 で追加する公開 API は Task 3 まで未使用のため、`#[allow(dead_code)]` を一時付与して clippy を通す（Task 3 で外す）
- 各タスク完了時に `cargo test` と `cargo clippy -- -D warnings` が通ること
- コミットメッセージ末尾: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: config — `[[graph]]` の追加と検証

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`
- Test: `src/config.rs`（`#[cfg(test)] mod tests` 内、既存 `write_tmp` パターン）

**Interfaces:**
- Consumes: 既存の `Config` / `ConfigError` / `write_tmp` テストヘルパ
- Produces: `pub struct Graph { name, label, unit, unit_daily, query }`・`Graph::label() -> &str`・`Graph::unit_for(period: &str) -> &str`・`Config::graphs: Vec<Graph>`・`Config::find_graph(&str) -> Option<&Graph>`・`ConfigError::{DuplicateGraph, EmptyGraphQuery, PeriodPlaceholder}`

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `mod tests` 末尾に追加:

```rust
    #[test]
    fn graph_parses() {
        let p = write_tmp(
            "graphok",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name       = "generation"
            label      = "太陽光発電"
            unit       = "W"
            unit_daily = "kWh"
            query      = ["embalse-query", "generation", "{period}"]
            [[graph]]
            name  = "co2"
            unit  = "ppm"
            query = ["embalse-query", "co2", "{period}"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let g = cfg.find_graph("generation").unwrap();
        assert_eq!(g.label(), "太陽光発電");
        assert_eq!(g.unit_for("today"), "W");
        assert_eq!(g.unit_for("week"), "kWh");
        assert_eq!(g.unit_for("month"), "kWh");
        let c = cfg.find_graph("co2").unwrap();
        assert_eq!(c.label(), "co2"); // label 未指定は name
        assert_eq!(c.unit_for("week"), "ppm"); // unit_daily 未指定は unit
        assert!(cfg.find_graph("nope").is_none());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_zero_entries_ok() {
        // [[graph]] 無しでも従来どおり起動できる（既存機能への影響ゼロ）。
        let p = write_tmp(
            "graphzero",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert!(cfg.graphs.is_empty());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_duplicate_name_rejected() {
        let p = write_tmp(
            "graphdup",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "g", "{period}"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "g", "{period}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateGraph(_))
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_empty_query_rejected() {
        let p = write_tmp(
            "graphempty",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = []
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::EmptyGraphQuery(_))
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_period_placeholder_zero_rejected() {
        let p = write_tmp(
            "graphp0",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "g", "today"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::PeriodPlaceholder { count: 0, .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn graph_period_placeholder_two_rejected() {
        let p = write_tmp(
            "graphp2",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name = "g"
            unit = "W"
            query = ["embalse-query", "{period}", "{period}"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::PeriodPlaceholder { count: 2, .. })
        ));
        std::fs::remove_file(p).ok();
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test config::tests::graph 2>&1 | tail -20`
Expected: コンパイルエラー（`Graph` / `find_graph` / `DuplicateGraph` 未定義）

- [ ] **Step 3: 実装**

`src/config.rs` の `Group` 定義の後（`fn default_bind` の前あたり）に追加:

```rust
/// embalse 等の読み出し CLI をテンプレで指定するグラフ定義（きろくセクション）。
/// mando はコマンド名を知らない — {period} を検証済み値に置換して exec するだけ
/// （設計原則 2: バックエンド非依存）。
#[derive(Debug, Clone, Deserialize)]
pub struct Graph {
    /// URL に使う識別子。
    pub name: String,
    /// UI 表示名（任意）。未指定なら name。
    #[serde(default, alias = "alias")]
    pub label: Option<String>,
    /// 今日ビュー（時系列カーブ）の単位表示。
    pub unit: String,
    /// 週/月ビュー（日別集計）の単位表示。未指定なら unit。
    #[serde(default)]
    pub unit_daily: Option<String>,
    /// exec するコマンドテンプレ。{period} プレースホルダをちょうど 1 個含む。
    pub query: Vec<String>,
}

// dead_code allow は一時措置 — Task 3（API ハンドラ）が使い始めたら外す。
// bin クレートでは #[cfg(test)] からの参照は dead_code を抑止しないため必要。
#[allow(dead_code)]
impl Graph {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    /// period に応じた表示単位（today は unit、週/月は unit_daily 優先）。
    pub fn unit_for(&self, period: &str) -> &str {
        if period == "today" {
            &self.unit
        } else {
            self.unit_daily.as_deref().unwrap_or(&self.unit)
        }
    }
}
```

`Config` struct にフィールド追加:

```rust
    #[serde(default, rename = "graph")]
    pub graphs: Vec<Graph>,
```

`ConfigError` に variant 追加:

```rust
    DuplicateGraph(String),
    EmptyGraphQuery(String),
    PeriodPlaceholder { graph: String, count: usize },
```

`Display` impl に追加:

```rust
            ConfigError::DuplicateGraph(n) => write!(f, "graph 名が重複: {n}"),
            ConfigError::EmptyGraphQuery(n) => write!(f, "graph {n}: query が空"),
            ConfigError::PeriodPlaceholder { graph, count } => {
                write!(f, "graph {graph}: query は {{period}} プレースホルダをちょうど 1 個含む必要がある（現在 {count} 個）")
            }
```

`validate()` の groups ループの後・`Ok(())` の前に追加:

```rust
        let mut seen_gr = std::collections::HashSet::new();
        for g in &self.graphs {
            if !seen_gr.insert(&g.name) {
                return Err(ConfigError::DuplicateGraph(g.name.clone()));
            }
            if g.query.is_empty() {
                return Err(ConfigError::EmptyGraphQuery(g.name.clone()));
            }
            let count: usize = g.query.iter().map(|s| s.matches("{period}").count()).sum();
            if count != 1 {
                return Err(ConfigError::PeriodPlaceholder {
                    graph: g.name.clone(),
                    count,
                });
            }
        }
```

`find_group` の後にメソッド追加:

```rust
    // dead_code allow は一時措置 — Task 3 が使い始めたら外す。
    #[allow(dead_code)]
    pub fn find_graph(&self, name: &str) -> Option<&Graph> {
        self.graphs.iter().find(|g| g.name == name)
    }
```

`config.example.toml` の末尾に追加:

```toml
# ── グラフ（きろくセクション）──────────────────────────────
# embalse 等の読み出し CLI をテンプレで指定する。{period} は today / week /
# month（検証済みの 3 値のみ）に置換して exec される。CLI は stdout に
#   [{"ts":"2026-07-15T10:00:00+09:00","series":"書斎","value":812.0}, ...]
# の JSON 配列を返すこと（series は単系列なら省略可）。SQL・スキーマ・
# データパスは embalse 側の責務で、mando はコマンド名すら知らない。
#
# [[graph]]
# name       = "generation"
# label      = "太陽光発電"
# unit       = "W"      # 今日ビュー（時系列カーブ）の単位
# unit_daily = "kWh"    # 週/月ビュー（日別集計）の単位。省略時 unit
# query      = ["embalse-query", "generation", "{period}"]
#
# [[graph]]
# name  = "co2"
# label = "CO2"
# unit  = "ppm"
# query = ["embalse-query", "co2", "{period}"]
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test config 2>&1 | tail -5`
Expected: 全 config テスト PASS（既存含む）

- [ ] **Step 5: clippy**

Run: `cargo clippy -- -D warnings 2>&1 | tail -3`
Expected: エラーなし

- [ ] **Step 6: コミット**

```bash
git add src/config.rs config.example.toml
git commit -m "feat: config に [[graph]] 定義と検証を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: normalize — 契約 JSON → チャート系列

**Files:**
- Modify: `src/normalize.rs`
- Test: `src/normalize.rs`（`mod tests` 内）

**Interfaces:**
- Consumes: `serde_json::Value`
- Produces: `pub struct GraphSeries { pub label: String, pub points: Vec<(String, f64)> }`（`Serialize` — points は JSON で `[["ts", v], ...]` になる）・`pub fn normalize_graph_rows(rows: &[Value], default_label: &str) -> Vec<GraphSeries>`

- [ ] **Step 1: 失敗するテストを書く**

`src/normalize.rs` の `mod tests` 末尾に追加:

```rust
    #[test]
    fn graph_rows_single_series_gets_default_label() {
        let rows = [
            json!({"ts": "2026-07-15T10:00:00+09:00", "value": 100.0}),
            json!({"ts": "2026-07-15T10:05:00+09:00", "value": 200.0}),
        ];
        let s = normalize_graph_rows(&rows, "太陽光発電");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].label, "太陽光発電");
        assert_eq!(s[0].points.len(), 2);
        assert_eq!(s[0].points[0].1, 100.0);
    }

    #[test]
    fn graph_rows_grouped_by_series_in_first_appearance_order() {
        let rows = [
            json!({"ts": "t1", "series": "書斎", "value": 800.0}),
            json!({"ts": "t1", "series": "リビング", "value": 600.0}),
            json!({"ts": "t2", "series": "書斎", "value": 820.0}),
        ];
        let s = normalize_graph_rows(&rows, "CO2");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].label, "書斎");
        assert_eq!(s[0].points.len(), 2);
        assert_eq!(s[1].label, "リビング");
        assert_eq!(s[1].points.len(), 1);
    }

    #[test]
    fn graph_rows_sorted_by_ts_ascending() {
        let rows = [
            json!({"ts": "2026-07-15T10:05:00+09:00", "value": 2.0}),
            json!({"ts": "2026-07-15T10:00:00+09:00", "value": 1.0}),
        ];
        let s = normalize_graph_rows(&rows, "x");
        assert_eq!(s[0].points[0].1, 1.0);
        assert_eq!(s[0].points[1].1, 2.0);
    }

    #[test]
    fn graph_rows_invalid_rows_dropped() {
        let rows = [
            json!({"ts": "t1", "value": "not-a-number"}), // value 非数値
            json!({"value": 1.0}),                         // ts 欠落
            json!({"ts": "t2"}),                           // value 欠落
            json!("garbage"),                              // オブジェクトですらない
            json!({"ts": "t3", "value": 3.0}),             // 唯一の正常行
        ];
        let s = normalize_graph_rows(&rows, "x");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].points, vec![("t3".to_string(), 3.0)]);
    }

    #[test]
    fn graph_rows_empty_is_empty() {
        assert!(normalize_graph_rows(&[], "x").is_empty());
    }

    #[test]
    fn graph_series_serializes_points_as_pairs() {
        let s = GraphSeries {
            label: "発電".into(),
            points: vec![("t1".into(), 1.5)],
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"label":"発電","points":[["t1",1.5]]}"#
        );
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test normalize::tests::graph 2>&1 | tail -5`
Expected: コンパイルエラー（`GraphSeries` / `normalize_graph_rows` 未定義）

- [ ] **Step 3: 実装**

`src/normalize.rs` の `normalize_mat_onoff` の後に追加:

```rust
/// グラフ 1 系列。契約 JSON の行を series 別に束ねたもの。
// dead_code allow は一時措置 — Task 3（API ハンドラ）が使い始めたら外す。
#[allow(dead_code)]
#[derive(Debug, PartialEq, Serialize)]
pub struct GraphSeries {
    pub label: String,
    /// (ts, value)。ts 昇順。JSON では [["ts", value], ...] になる。
    pub points: Vec<(String, f64)>,
}

/// embalse 読み出し CLI の契約 JSON（フラット行配列）→ チャート系列。
///
/// 契約: `[{"ts": "ISO8601", "series": "ラベル(任意)", "value": 数値}, ...]`
/// series 省略行は default_label（グラフの表示名）に束ねる。ts / value が
/// 欠けた・型不正の行は drop（部分的に壊れたデータで全体を落とさない）。
/// 系列は初出順、各系列内は ts 昇順（同一オフセットの ISO8601 は辞書順=時刻順）。
/// 下層（embalse）の出力形式に関する知識はこの関数に閉じる（設計原則 4）。
// dead_code allow は一時措置 — Task 3（API ハンドラ）が使い始めたら外す。
#[allow(dead_code)]
pub fn normalize_graph_rows(rows: &[Value], default_label: &str) -> Vec<GraphSeries> {
    let mut order: Vec<String> = Vec::new();
    let mut by_label: std::collections::HashMap<String, Vec<(String, f64)>> =
        std::collections::HashMap::new();
    for row in rows {
        let Some(ts) = row.get("ts").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = row.get("value").and_then(Value::as_f64) else {
            continue;
        };
        let label = row
            .get("series")
            .and_then(Value::as_str)
            .unwrap_or(default_label);
        if !by_label.contains_key(label) {
            order.push(label.to_string());
        }
        by_label
            .entry(label.to_string())
            .or_default()
            .push((ts.to_string(), value));
    }
    order
        .into_iter()
        .map(|label| {
            let mut points = by_label.remove(&label).unwrap_or_default();
            points.sort_by(|a, b| a.0.cmp(&b.0));
            GraphSeries { label, points }
        })
        .collect()
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test normalize 2>&1 | tail -5`
Expected: 全 normalize テスト PASS

- [ ] **Step 5: clippy**

Run: `cargo clippy -- -D warnings 2>&1 | tail -3`
Expected: エラーなし（`#[allow(dead_code)]` を付与済みのため）

- [ ] **Step 6: コミット**

```bash
git add src/normalize.rs
git commit -m "feat: 契約 JSON をチャート系列へ正規化する normalize_graph_rows を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: API — GET /api/graphs と GET /api/graphs/{name}

**Files:**
- Modify: `src/main.rs`
- Modify: `CLAUDE.md`（安定ミニ API 一覧に 2 行追記）
- Test: `src/main.rs`（`mod tests` 内、既存 `call` / `test_app` パターン）

**Interfaces:**
- Consumes: Task 1 の `Config::graphs` / `find_graph` / `Graph::{label, unit_for}`、Task 2 の `normalize_graph_rows` / `GraphSeries`、既存 `Executor` / `ExecOutcome` / `json_str` / `not_found` パターン
- Produces: `GET /api/graphs` → `[{"name","label"}]`、`GET /api/graphs/{name}?period=` → `{"name","period","unit","series":[{"label","points":[["ts",v],...]}]}`。`App` に `graph_executor: Executor` フィールド追加（**テストの `test_app()` と `main()` の両方で初期化**）

- [ ] **Step 1: 失敗するテストを書く**

`src/main.rs` の `mod tests` 内、`test_app()` の config 文字列末尾（`"##,` の直前）にグラフ定義を追加:

```toml
            [[graph]]
            name       = "generation"
            label      = "太陽光発電"
            unit       = "W"
            unit_daily = "kWh"
            query      = ["sh", "-c", "printf '[{\"ts\":\"2026-07-15T10:05:00+09:00\",\"value\":200},{\"ts\":\"2026-07-15T10:00:00+09:00\",\"value\":100}]'", "sh", "{period}"]
            [[graph]]
            name  = "co2"
            label = "CO2"
            unit  = "ppm"
            query = ["sh", "-c", "printf '[{\"ts\":\"t1\",\"series\":\"書斎\",\"value\":800},{\"ts\":\"t1\",\"series\":\"リビング\",\"value\":600}]'", "sh", "{period}"]
            [[graph]]
            name  = "strict"
            unit  = "W"
            query = ["sh", "-c", "test \"$1\" = today && printf '[]'", "sh", "{period}"]
            [[graph]]
            name  = "broken"
            unit  = "W"
            query = ["sh", "-c", "exit 1", "sh", "{period}"]
            [[graph]]
            name  = "garbage"
            unit  = "W"
            query = ["sh", "-c", "printf 'not-json'", "sh", "{period}"]
            [[graph]]
            name  = "notarray"
            unit  = "W"
            query = ["sh", "-c", "printf '{}'", "sh", "{period}"]
```

テストを `mod tests` 末尾に追加:

```rust
    #[tokio::test]
    async fn graphs_list_has_name_and_label() {
        let (st, v) = call("GET", "/api/graphs").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let gen = arr.iter().find(|g| g["name"] == "generation").unwrap();
        assert_eq!(gen["label"], "太陽光発電");
    }

    #[tokio::test]
    async fn graph_today_normalized_and_sorted() {
        let (st, v) = call("GET", "/api/graphs/generation").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["name"], "generation");
        assert_eq!(v["period"], "today"); // period 未指定は today
        assert_eq!(v["unit"], "W");
        let s = &v["series"][0];
        assert_eq!(s["label"], "太陽光発電"); // series 省略行は graph label に束ねる
        // ts 昇順にソートされる（スタブは逆順で返す）。
        assert_eq!(s["points"][0][1], 100.0);
        assert_eq!(s["points"][1][1], 200.0);
    }

    #[tokio::test]
    async fn graph_week_uses_unit_daily() {
        let (st, v) = call("GET", "/api/graphs/generation?period=week").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["period"], "week");
        assert_eq!(v["unit"], "kWh");
    }

    #[tokio::test]
    async fn graph_multi_series_first_appearance_order() {
        let (st, v) = call("GET", "/api/graphs/co2").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["unit"], "ppm"); // unit_daily 未指定は unit
        let arr = v["series"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["label"], "書斎");
        assert_eq!(arr[1]["label"], "リビング");
    }

    #[tokio::test]
    async fn graph_period_substitution_reaches_argv() {
        // 偽装 sh は "$1" = "today" のときだけ成功する。
        let (st, v) = call("GET", "/api/graphs/strict?period=today").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["series"].as_array().unwrap().len(), 0); // 0 行 → 200 + 空 series
        // week を送ると置換値がそのまま argv に渡っていれば exit 1 → 502。
        let (st, _) = call("GET", "/api/graphs/strict?period=week").await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn graph_invalid_period_is_400() {
        for p in ["yesterday", "TODAY", "today%20x", ""] {
            let (st, _) = call("GET", &format!("/api/graphs/generation?period={p}")).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "period: {p}");
        }
    }

    #[tokio::test]
    async fn graph_unknown_is_404() {
        let (st, v) = call("GET", "/api/graphs/ghost").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(v["error"], "unknown graph");
    }

    #[tokio::test]
    async fn graph_exec_failure_is_502() {
        let (st, v) = call("GET", "/api/graphs/broken").await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
        assert_eq!(v["error"], "graph query failed");
    }

    #[tokio::test]
    async fn graph_non_json_stdout_is_502() {
        let (st, _) = call("GET", "/api/graphs/garbage").await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
        let (st, _) = call("GET", "/api/graphs/notarray").await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
    }
```

注意: `graph_invalid_period_is_400` の `period=`（空文字）は `Some("")` にデシリアライズされ、enum 検証で 400 になる想定。

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test tests::graph 2>&1 | tail -10`
Expected: コンパイルエラー（`graph_executor` フィールド不足 → まずここで `test_app` が落ちる）

- [ ] **Step 3: 実装**

`src/main.rs` の変更点:

use に `Query` を追加:

```rust
use axum::{
    extract::{Path, Query, State},
    ...
```

normalize の use に追加:

```rust
use normalize::{normalize_enl_state, normalize_mat_onoff, GraphSeries, State as DeviceState};
```

`App` にフィールド追加（コメント込み）:

```rust
struct App {
    config: Config,
    executor: Executor,
    /// グラフ読み出し専用の直列化器。devices の executor（3610 衝突対策）とは
    /// 別枠 — 重い読み出し（duckdb 等）がシャッター操作をブロックしないため。
    /// グラフ同士は直列（ホストの CPU/メモリ保護）。
    graph_executor: Executor,
}
```

`main()` の `App` 構築に `graph_executor: Executor::new(),` を追加。`mod tests` の `test_app()` も同様に追加。

`router()` に追加:

```rust
        .route("/api/graphs", get(list_graphs))
        .route("/api/graphs/:name", get(get_graph))
```

ハンドラ群（`list_groups` の後あたりに追加）:

```rust
#[derive(Serialize)]
struct GraphInfo {
    name: String,
    label: String,
}

async fn list_graphs(State(app): State<Shared>) -> Json<Vec<GraphInfo>> {
    let graphs = app
        .config
        .graphs
        .iter()
        .map(|g| GraphInfo {
            name: g.name.clone(),
            label: g.label().to_string(),
        })
        .collect();
    Json(graphs)
}

#[derive(Deserialize)]
struct GraphQuery {
    period: Option<String>,
}

#[derive(Serialize)]
struct GraphView {
    name: String,
    period: String,
    unit: String,
    series: Vec<GraphSeries>,
}

/// グラフデータ取得。query テンプレの {period} を検証済み値に置換して exec し、
/// 契約 JSON（フラット行配列）をチャート系列へ正規化して返す。
async fn get_graph(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<GraphQuery>,
) -> Response {
    let Some(graph) = app.config.find_graph(&name) else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unknown graph\",\"name\":{}}}",
                json_str(&name)
            ),
        )
            .into_response();
    };
    // enum 検証してからテンプレ置換する（任意文字列を subprocess に渡さない）。
    let period = q.period.as_deref().unwrap_or("today");
    if !matches!(period, "today" | "week" | "month") {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"invalid period\",\"period\":{}}}",
                json_str(period)
            ),
        )
            .into_response();
    }
    let cmd: Vec<String> = graph
        .query
        .iter()
        .map(|s| s.replace("{period}", period))
        .collect();
    let result = app.graph_executor.run(&cmd).await;
    if result.outcome != ExecOutcome::Success {
        tracing::warn!(
            graph = %name,
            outcome = ?result.outcome,
            stderr = %result.stderr.trim(),
            "graph query 非成功"
        );
        return graph_unavailable(&name);
    }
    let rows = match serde_json::from_str::<Value>(&result.stdout) {
        Ok(Value::Array(rows)) => rows,
        // 配列以外・パース不能は契約違反（原則 7: 誤魔化さず 502 で正直に返す）。
        Ok(_) | Err(_) => {
            tracing::warn!(graph = %name, "graph query の stdout が契約 JSON 配列でない");
            return graph_unavailable(&name);
        }
    };
    let series = normalize::normalize_graph_rows(&rows, graph.label());
    Json(GraphView {
        name: graph.name.clone(),
        period: period.to_string(),
        unit: graph.unit_for(period).to_string(),
        series,
    })
    .into_response()
}

/// 下層の読み出しに失敗（exec 非成功・契約 JSON でない）→ 502。
fn graph_unavailable(name: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        [(header::CONTENT_TYPE, "application/json")],
        format!(
            "{{\"error\":\"graph query failed\",\"name\":{}}}",
            json_str(name)
        ),
    )
        .into_response()
}
```

Task 1・2 で一時付与した `#[allow(dead_code)]`（`src/config.rs` の `impl Graph`・`find_graph`、`src/normalize.rs` の `GraphSeries`・`normalize_graph_rows`）をすべて外す（このタスクで使用され始めるため。付随コメントも削除）。

`CLAUDE.md` の「## やること（安定ミニ API）」リスト末尾に追加:

```markdown
- `GET  /api/graphs` — config 上のグラフ一覧（きろくセクション）
- `GET  /api/graphs/{name}?period=today|week|month` — graph query テンプレを exec → 正規化した系列 `{ "series": [...] }`
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test 2>&1 | tail -5`
Expected: 全テスト PASS

- [ ] **Step 5: clippy**

Run: `cargo clippy -- -D warnings 2>&1 | tail -3`
Expected: エラーなし

- [ ] **Step 6: コミット**

```bash
git add src/main.rs src/normalize.rs CLAUDE.md
git commit -m "feat: GET /api/graphs と GET /api/graphs/{name}?period= を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: UI — きろくセクション（期間タブ + SVG チャート）

**Files:**
- Modify: `index.html`
- 検証用スタブ: `/tmp/claude-1000/-home-noguk-ghq-github-com-nogu3-mando/9f1b9e9f-418c-4b19-8283-111b67a810bc/scratchpad/graph-stub-config.toml`（コミットしない）

**Interfaces:**
- Consumes: Task 3 の `GET /api/graphs`（`[{name,label}]`）と `GET /api/graphs/{name}?period=`（`{name,period,unit,series:[{label,points:[["ts",v],...]}]}`）。既存の `api()` / `sectionHeading()` / `.spin` / CSS 変数
- Produces: なし（最終タスク）

**設計メモ（dataviz 指針の適用）:**
- 系列色 `["#3987e5", "#008300", "#d55181", "#c98500"]` — mando ダーク面 `#10141d` で validator 全チェック PASS 済み。初出順に固定割当、循環させない（5 系列以上は先頭 4 つまで描画して残りは無視 — config 側で分割する運用）
- 折れ線 2px round join/cap・終端マーカ r=4 + サーフェスリング 2px・棒は上端 4px 丸め/基線は角・棒幅 ≤24px・隣接間隔 ≥2px・罫線は hairline 実線で控えめ
- 凡例は 2 系列以上のときだけ（単系列はカードタイトルが系列名を兼ねる）。テキストは常に text token 色（--fg / --muted）、系列色は色玉が背負う
- ホバー層: pointermove / pointerdown で最寄り時点の値を subtitle 行に表示（スマホはドラッグで読める）
- 今日 = 折れ線、週/月 = 単系列なら棒・複数系列なら折れ線（30 日 × 3 系列のグループ棒はスマホで潰れるため）

- [ ] **Step 1: CSS を追加**

`index.html` の `<style>` 内、`.boot` の前に追加:

```css
  /* ── きろく（embalse グラフ）─────────────────── */
  .sec.krow { display: flex; align-items: center; gap: 10px; }
  .ptabs {
    display: flex; gap: 2px; margin-left: auto;
    background: rgba(255,255,255,.05); border: 1px solid var(--line);
    border-radius: 999px; padding: 3px;
  }
  .ptabs button {
    appearance: none; border: none; border-radius: 999px;
    background: none; color: var(--muted); font-size: 12px; font-weight: 700;
    padding: 6px 14px; cursor: pointer; touch-action: manipulation; user-select: none;
    transition: background var(--tap), color var(--tap);
  }
  .ptabs button.sel { background: var(--accent); color: #0b1020; }
  .gcard {
    background: var(--panel); border: 1px solid var(--line);
    border-radius: 16px; padding: 12px 14px; margin: 10px 0;
    box-shadow: 0 4px 16px rgba(0,0,0,.28), inset 0 1px 0 rgba(255,255,255,.04);
    backdrop-filter: blur(10px); -webkit-backdrop-filter: blur(10px);
    animation: rise .35s cubic-bezier(.2,.8,.3,1) both;
  }
  .gcard .ghd { display: flex; align-items: baseline; gap: 8px; }
  .gcard .gtitle { font-size: 14px; font-weight: 700; }
  .gcard .gval { margin-left: auto; font-size: 16px; font-weight: 800; }
  .gcard .gval .u { font-size: 11px; color: var(--muted); font-weight: 700; margin-left: 3px; }
  .gcard .gsub { font-size: 11px; color: var(--muted); min-height: 15px; margin-top: 2px; }
  .gcard .glegend {
    display: flex; flex-wrap: wrap; gap: 4px 14px; margin-top: 6px;
    font-size: 11px; color: var(--muted);
  }
  .gcard .glegend .k { display: inline-flex; align-items: center; gap: 5px; }
  .gcard .glegend .kd { width: 9px; height: 9px; border-radius: 50%; flex: none; }
  .gcard svg { display: block; width: 100%; height: auto; margin-top: 8px; touch-action: pan-y; }
  .gcard .gmsg { font-size: 12px; color: var(--muted); padding: 22px 0; text-align: center; }
  .gcard .gmsg.error { color: var(--warn); }
```

- [ ] **Step 2: JS を追加**

`index.html` の `<script>` 内、`boot()` の定義の前に追加:

```js
/* ── きろく（embalse グラフ）─────────────────────── */
// dataviz 検証済みカテゴリカルパレット（ダーク面 #10141d で全チェック PASS）。
// 系列へ初出順に固定割当する。循環させない — 5 系列以上は先頭 4 系列のみ描画
// （必要なら config 側でグラフを分割する）。
const GRAPH_COLORS = ["#3987e5", "#008300", "#d55181", "#c98500"];
const GRAPH_SURFACE = "#10141d"; // マーカのサーフェスリング用（パネルの実効色）
const PERIODS = [["today", "今日"], ["week", "週"], ["month", "月"]];
let graphPeriod = "today";
const graphCards = new Map(); // name -> {valEl, subEl, legendEl, bodyEl}

function buildGraphSection(graphList) {
  const head = sectionHeading("📈 きろく");
  head.classList.add("krow");
  const tabs = document.createElement("div");
  tabs.className = "ptabs";
  for (const [p, label] of PERIODS) {
    const b = document.createElement("button");
    b.type = "button"; b.textContent = label; b.dataset.period = p;
    if (p === graphPeriod) b.classList.add("sel");
    b.addEventListener("click", () => {
      if (graphPeriod === p) return;
      graphPeriod = p;
      for (const t of tabs.querySelectorAll("button")) {
        t.classList.toggle("sel", t.dataset.period === p);
      }
      loadGraphs();
    });
    tabs.appendChild(b);
  }
  head.appendChild(tabs);
  app.appendChild(head);

  for (const g of graphList) {
    const card = document.createElement("div");
    card.className = "gcard";
    card.innerHTML = `
      <div class="ghd"><span class="gtitle"></span><span class="gval"></span></div>
      <div class="gsub"></div>
      <div class="glegend" hidden></div>
      <div class="gbody"><div class="gmsg">…</div></div>
    `;
    card.querySelector(".gtitle").textContent = g.label;
    graphCards.set(g.name, {
      valEl: card.querySelector(".gval"),
      subEl: card.querySelector(".gsub"),
      legendEl: card.querySelector(".glegend"),
      bodyEl: card.querySelector(".gbody"),
    });
    app.appendChild(card);
  }
  // 取得はセクションが初めて画面に入ったときまで遅らせる
  // （開いてすぐの操作を重い読み出しで邪魔しない・ポーリングはしない）。
  const io = new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting)) {
      io.disconnect();
      loadGraphs();
    }
  }, { rootMargin: "80px" });
  io.observe(head);
}

async function loadGraphs() {
  const period = graphPeriod;
  for (const [name, gc] of graphCards) {
    gc.bodyEl.innerHTML = `<div class="gmsg"><span class="spin"></span>読み込み中…</div>`;
    gc.valEl.textContent = "";
    gc.subEl.textContent = "";
    gc.legendEl.hidden = true;
    try {
      const view = await api(
        "GET",
        `/api/graphs/${encodeURIComponent(name)}?period=${period}`
      );
      if (period !== graphPeriod) return; // タブが切り替わっていたら破棄
      renderGraph(gc, view);
    } catch (e) {
      if (period !== graphPeriod) return;
      // 原則 7: 取得できないものは取得できないと言う（ゼロ埋め・補間で誤魔化さない）。
      gc.bodyEl.innerHTML = `<div class="gmsg error">データを取得できませんでした</div>`;
    }
  }
}

function renderGraph(gc, view) {
  const series = view.series.filter((s) => s.points.length).slice(0, GRAPH_COLORS.length);
  if (!series.length) {
    gc.bodyEl.innerHTML = `<div class="gmsg">まだデータがありません</div>`;
    return;
  }
  // 見出しの要約数値 = 先頭系列の最新値（数字が主、カーブは従）。
  const first = series[0];
  const latest = first.points[first.points.length - 1][1];
  gc.valEl.textContent = fmtNum(latest);
  const u = document.createElement("span");
  u.className = "u";
  u.textContent = view.unit;
  gc.valEl.appendChild(u);
  // generation の今日ビューは合計 kWh を併記（5 分ビン W の積分近似 Σv×(5/60)÷1000）。
  if (view.name === "generation" && view.period === "today") {
    const kwh = (first.points.reduce((a, p) => a + p[1], 0) * (5 / 60)) / 1000;
    gc.subEl.textContent = `今日の合計 約 ${kwh.toFixed(1)} kWh`;
  }
  // 凡例は 2 系列以上のときだけ（単系列はタイトルが系列名を兼ねる）。
  gc.legendEl.innerHTML = "";
  gc.legendEl.hidden = series.length < 2;
  if (series.length >= 2) {
    series.forEach((s, i) => {
      const k = document.createElement("span");
      k.className = "k";
      const d = document.createElement("span");
      d.className = "kd";
      d.style.background = GRAPH_COLORS[i];
      k.append(d, s.label);
      gc.legendEl.appendChild(k);
    });
  }
  gc.bodyEl.innerHTML = "";
  // 今日 = 時系列カーブ。週/月 = 日別集計（単系列は棒、複数系列は折れ線 —
  // 30 日 × 複数系列のグループ棒はスマホで潰れる）。
  const svg =
    view.period !== "today" && series.length === 1
      ? drawBarChart(series[0], view.period)
      : drawLineChart(series, view.period);
  gc.bodyEl.appendChild(svg);
  attachReadout(svg, gc, series, view);
}

/* ── SVG チャート描画（手描き・外部ライブラリなし）───── */
const SVG_NS = "http://www.w3.org/2000/svg";
const PLOT = { w: 320, h: 132, l: 40, r: 8, t: 8, b: 18 };

function svgNode(tag, attrs) {
  const e = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  return e;
}

/* 軸の上端を切りの良い数へ丸める（1/2/5 × 10^n）。 */
function niceCeil(v) {
  if (v <= 0) return 1;
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 5, 10]) if (v <= m * p) return m * p;
  return 10 * p;
}

function fmtNum(v) {
  return Math.abs(v) >= 1000
    ? Math.round(v).toLocaleString("ja-JP")
    : String(Math.round(v * 10) / 10);
}

function fmtTs(d, period) {
  if (period === "today") {
    return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
  }
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

/* 罫線（hairline・控えめ）+ y 目盛 + x 端ラベルの共通描画。 */
function drawFrame(svg, py, yMin, yMax, xLabels) {
  const { w, h, l, r, b } = PLOT;
  for (const f of [0, 0.5, 1]) {
    const v = yMin + (yMax - yMin) * f;
    const y = py(v);
    svg.appendChild(svgNode("line", {
      x1: l, y1: y, x2: w - r, y2: y,
      stroke: "rgba(150,170,210,0.12)", "stroke-width": 1,
    }));
    const t = svgNode("text", {
      x: l - 5, y: y + 3, "text-anchor": "end",
      "font-size": 9, fill: "var(--muted)",
    });
    t.textContent = fmtNum(v);
    svg.appendChild(t);
  }
  for (const [x, anchor, text] of xLabels) {
    const t = svgNode("text", {
      x, y: h - b + 13, "text-anchor": anchor,
      "font-size": 9, fill: "var(--muted)",
    });
    t.textContent = text;
    svg.appendChild(t);
  }
}

function drawLineChart(series, period) {
  const { w, h, l, r, t, b } = PLOT;
  const svg = svgNode("svg", { viewBox: `0 0 ${w} ${h}`, role: "img" });
  const xs = series.flatMap((s) => s.points.map((p) => Date.parse(p[0])));
  const vs = series.flatMap((s) => s.points.map((p) => p[1]));
  const x0 = Math.min(...xs), x1 = Math.max(...xs);
  const yMin = Math.min(0, ...vs);
  const yMax = niceCeil(Math.max(...vs, 1));
  const px = (x) =>
    x1 === x0 ? l + (w - l - r) / 2 : l + ((x - x0) / (x1 - x0)) * (w - l - r);
  const py = (v) => t + (1 - (v - yMin) / (yMax - yMin)) * (h - t - b);
  drawFrame(svg, py, yMin, yMax, [
    [px(x0), "start", fmtTs(new Date(x0), period)],
    [px(x1), "end", fmtTs(new Date(x1), period)],
  ]);
  series.forEach((s, i) => {
    const color = GRAPH_COLORS[i];
    const d = s.points
      .map((p, j) => `${j ? "L" : "M"}${px(Date.parse(p[0])).toFixed(1)},${py(p[1]).toFixed(1)}`)
      .join("");
    svg.appendChild(svgNode("path", {
      d, fill: "none", stroke: color, "stroke-width": 2,
      "stroke-linejoin": "round", "stroke-linecap": "round",
    }));
    // 終端マーカ（8px）+ サーフェスリング 2px — 線と重なっても読める。
    const lp = s.points[s.points.length - 1];
    svg.appendChild(svgNode("circle", {
      cx: px(Date.parse(lp[0])).toFixed(1), cy: py(lp[1]).toFixed(1),
      r: 4, fill: color, stroke: GRAPH_SURFACE, "stroke-width": 2,
    }));
  });
  return svg;
}

function drawBarChart(s, period) {
  const { w, h, l, r, t, b } = PLOT;
  const svg = svgNode("svg", { viewBox: `0 0 ${w} ${h}`, role: "img" });
  const n = s.points.length;
  const yMax = niceCeil(Math.max(...s.points.map((p) => p[1]), 1));
  const py = (v) => t + (1 - v / yMax) * (h - t - b);
  const slot = (w - l - r) / n;
  // 棒 ≤24px・隣接とは最低 2px の地の間隔。
  const bw = Math.max(2, Math.min(24, slot - 2));
  const first = s.points[0], last = s.points[n - 1];
  drawFrame(svg, py, 0, yMax, [
    [l + slot / 2, "start", fmtTs(new Date(first[0]), period)],
    [l + slot * (n - 0.5), "end", fmtTs(new Date(last[0]), period)],
  ]);
  const y0 = py(0);
  s.points.forEach((p, i) => {
    const x = l + slot * i + (slot - bw) / 2;
    const y = py(p[1]);
    // 上端 4px 丸め・基線は角（データ端だけ丸める）。
    const rr = Math.min(4, bw / 2, Math.max(0, y0 - y));
    svg.appendChild(svgNode("path", {
      d: `M${x},${y0} L${x},${y + rr} Q${x},${y} ${x + rr},${y}` +
         ` L${x + bw - rr},${y} Q${x + bw},${y} ${x + bw},${y + rr} L${x + bw},${y0} Z`,
      fill: GRAPH_COLORS[0],
    }));
  });
  return svg;
}

/* ── ホバー/タッチ読み取り層: 最寄り時点の値を gsub に表示 ── */
// 系列間の時点対応は index ベースの近似（同一クエリ由来のビンは揃っている前提）。
function attachReadout(svg, gc, series, view) {
  const baseSub = gc.subEl.textContent;
  const pts0 = series[0].points;
  const show = (ev) => {
    const rect = svg.getBoundingClientRect();
    const fx = Math.max(0, Math.min(1, (ev.clientX - rect.left) / rect.width));
    const idx = Math.round(fx * (pts0.length - 1));
    const ts = new Date(Date.parse(pts0[idx][0]));
    const vals = series.map((s, i) => {
      const p = s.points[Math.min(idx, s.points.length - 1)];
      return (series.length > 1 ? s.label + " " : "") + fmtNum(p[1]);
    });
    gc.subEl.textContent =
      `${fmtTs(ts, view.period)}  ${vals.join(" / ")} ${view.unit}`;
  };
  svg.addEventListener("pointerdown", show);
  svg.addEventListener("pointermove", show);
  svg.addEventListener("pointerleave", () => { gc.subEl.textContent = baseSub; });
  svg.addEventListener("pointerup", () => { gc.subEl.textContent = baseSub; });
}
```

- [ ] **Step 3: boot() を配線**

`boot()` 冒頭の `Promise.all` を差し替え:

```js
  let devices, grps, graphList;
  try {
    [devices, grps, graphList] = await Promise.all([
      api("GET", "/api/devices"),
      api("GET", "/api/groups").catch(() => []),
      api("GET", "/api/graphs").catch(() => []),
    ]);
  } catch (e) {
    app.innerHTML = `<div class="boot">⚠ サーバに接続できません</div>`;
    return;
  }
```

`boot()` の shutter セクションの後・`startPolling();` の前に追加:

```js
  // グラフが config に無ければセクションごと出さない。
  if (graphList.length) buildGraphSection(graphList);
```

- [ ] **Step 4: ビルドとテスト**

Run: `cargo build 2>&1 | tail -3 && cargo test 2>&1 | tail -3 && cargo clippy -- -D warnings 2>&1 | tail -3`
Expected: すべて成功（index.html は `include_str!` なのでビルドが通れば焼き込みは成立）

- [ ] **Step 5: スタブ config で動作確認**

スタブ config を scratchpad に書く（`graph-stub-config.toml`。デバイス 1 個 + グラフ 3 種、`{period}` で分岐するダミーデータ）:

```toml
bind = "127.0.0.1:8899"

[[device]]
name = "shutter"
label = "テスト"
get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
open  = ["sh", "-c", "printf '{}'"]
close = ["sh", "-c", "printf '{}'"]

[[graph]]
name       = "generation"
label      = "太陽光発電"
unit       = "W"
unit_daily = "kWh"
query = ["sh", "-c", """
case "$1" in
today) printf '[{"ts":"2026-07-16T06:00:00+09:00","value":0},{"ts":"2026-07-16T07:00:00+09:00","value":320},{"ts":"2026-07-16T08:00:00+09:00","value":900},{"ts":"2026-07-16T09:00:00+09:00","value":1800},{"ts":"2026-07-16T10:00:00+09:00","value":2600},{"ts":"2026-07-16T11:00:00+09:00","value":3100},{"ts":"2026-07-16T12:00:00+09:00","value":2900}]' ;;
*) printf '[{"ts":"2026-07-10T00:00:00+09:00","value":12.1},{"ts":"2026-07-11T00:00:00+09:00","value":8.4},{"ts":"2026-07-12T00:00:00+09:00","value":15.0},{"ts":"2026-07-13T00:00:00+09:00","value":3.2},{"ts":"2026-07-14T00:00:00+09:00","value":11.7},{"ts":"2026-07-15T00:00:00+09:00","value":14.2},{"ts":"2026-07-16T00:00:00+09:00","value":6.8}]' ;;
esac
""", "sh", "{period}"]

[[graph]]
name  = "co2"
label = "CO2"
unit  = "ppm"
query = ["sh", "-c", """
printf '[{"ts":"2026-07-16T09:00:00+09:00","series":"書斎","value":720},{"ts":"2026-07-16T10:00:00+09:00","series":"書斎","value":850},{"ts":"2026-07-16T11:00:00+09:00","series":"書斎","value":980},{"ts":"2026-07-16T12:00:00+09:00","series":"書斎","value":812},{"ts":"2026-07-16T09:00:00+09:00","series":"リビング","value":540},{"ts":"2026-07-16T10:00:00+09:00","series":"リビング","value":600},{"ts":"2026-07-16T11:00:00+09:00","series":"リビング","value":640},{"ts":"2026-07-16T12:00:00+09:00","series":"リビング","value":580}]'
""", "sh", "{period}"]

[[graph]]
name  = "broken"
label = "エラー確認用"
unit  = "W"
query = ["sh", "-c", "exit 1", "sh", "{period}"]
```

Run（サーバをバックグラウンド起動して curl 確認）:

```bash
MANDO_CONFIG=<scratchpad>/graph-stub-config.toml ./target/debug/mando &
sleep 1
curl -s http://127.0.0.1:8899/api/graphs | head -c 300; echo
curl -s "http://127.0.0.1:8899/api/graphs/generation?period=today" | head -c 300; echo
curl -s "http://127.0.0.1:8899/api/graphs/generation?period=week" | head -c 300; echo
curl -s -o /dev/null -w "%{http_code}\n" "http://127.0.0.1:8899/api/graphs/broken"
curl -s -o /dev/null -w "%{http_code}\n" "http://127.0.0.1:8899/api/graphs/generation?period=bad"
```

Expected: 一覧 3 件 / today は W の系列 / week は unit "kWh" / broken は 502 / bad は 400

- [ ] **Step 6: 視覚確認**

`http://127.0.0.1:8899/` をブラウザ（またはスクリーンショットツール）で開き、以下を目視確認して結果を報告する:

- 「📈 きろく」セクションがシャッターの下に出る。期間タブ 今日/週/月
- 太陽光発電: 折れ線 + 見出しに最新値 + 「今日の合計 約 x.x kWh」
- 週タブ: 棒グラフ（上端丸め・基線角・棒間に隙間）、単位 kWh
- CO2: 2 系列の折れ線 + 凡例（書斎/リビング）、テキストは muted 色
- エラー確認用: 「データを取得できませんでした」
- チャート上をドラッグすると subtitle 行に時刻と値が出て、離すと戻る
- ラベルの衝突・はみ出しがない

確認後、サーバを停止（`kill %1` 等）。

- [ ] **Step 7: コミット**

```bash
git add index.html
git commit -m "feat: きろくセクション（期間タブ + SVG チャート）を追加

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## スコープ外（この計画ではやらない）

- embalse 側の `embalse-query` CLI と SQL — embalse リポジトリで別途計画（スペックの CLI 出力契約が仕様書）
- jarvis での実データ結合確認 — embalse 側の実装完了後
- 短 TTL キャッシュ・水道/ガスグラフ — 必要になってから（スペック「やらないこと」）
