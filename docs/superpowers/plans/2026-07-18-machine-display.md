# jarvis マシン情報表示（machine グラフ + health バナー）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** embalse の `machine` グラフを「きろく」に出せるようにし（series 名の日本語化込み）、`embalse-query health` の結果を異常時のみ画面上部バナーで表示する。

**Architecture:** spec は `docs/superpowers/specs/2026-07-18-machine-display-design.md`。mando 本体は (1) `[[graph]]` に汎用 `series_labels` マップを追加、(2) 新設 `[health]` セクション + `GET /api/health`（グラフ用 Executor に相乗り）+ 正規化関数、(3) index.html に異常時のみのバナー。embalse の metric 名の知識はすべて config に留める。

**Tech Stack:** Rust (axum / serde / toml)、vanilla JS（index.html 焼き込み）。テストは cargo test（`sh -c 'printf ...'` スタブ）。

## Global Constraints

- 設計原則（プロジェクト CLAUDE.md）: バックエンド非依存 — mando 本体は `embalse-query` という名前も metric 名（`cpu_used_pct` 等）も知らない。知るのは config と正規化の入口だけ
- health のしきい値判定は embalse の責務 — mando は判定しない（level を写すだけ）
- `worst` の深刻度順位は `crit > stale > warn > ok`（stale = 収集停止も気づくべき異常）
- items が空（契約 0 行 / 全行 drop）なら `worst = "stale"`
- UI: health は boot 時 1 回 fetch のみ。ポーリングしない。fetch 失敗・404 は黙って出さない（console.error のみ）
- spec からの追加 1 点: `[health].label`（任意、例 "jarvis"）。UI に対象名を焼き込まないための表示名で、未指定なら UI は名前なしでバナーを出す
- 各タスク完了時: `cargo test` 全緑 + `cargo clippy -- -D warnings` クリーン
- コミットメッセージ末尾: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 1: config — `Graph.series_labels`

**Files:**
- Modify: `src/config.rs:76-89`（Graph 構造体）
- Modify: `config.example.toml`（graph 例の直後にコメント例を追記）
- Test: `src/config.rs`（tests モジュール）

**Interfaces:**
- Produces: `Graph.series_labels: Option<std::collections::HashMap<String, String>>`（Task 2 が `graph.series_labels.as_ref()` で使う）

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の tests モジュール末尾（`graph_period_placeholder_two_rejected` の後）に追加:

```rust
    #[test]
    fn graph_series_labels_parses() {
        let p = write_tmp(
            "graphlabels",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [[graph]]
            name  = "plain"
            unit  = "W"
            query = ["embalse-query", "plain", "{period}"]
            [[graph]]
            name  = "machine"
            label = "jarvis"
            unit  = "%"
            query = ["embalse-query", "machine", "{period}"]
            [graph.series_labels]
            cpu_used_pct = "CPU (%)"
            cpu_temp_c   = "温度 (℃)"
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let g = cfg.find_graph("machine").unwrap();
        let m = g.series_labels.as_ref().unwrap();
        assert_eq!(m.get("cpu_used_pct").unwrap(), "CPU (%)");
        assert_eq!(m.get("cpu_temp_c").unwrap(), "温度 (℃)");
        // series_labels 無しの既存形は None のまま
        assert!(cfg.find_graph("plain").unwrap().series_labels.is_none());
        std::fs::remove_file(p).ok();
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test graph_series_labels_parses`
Expected: コンパイルエラー（`series_labels` フィールドが存在しない）

- [ ] **Step 3: 最小実装**

`src/config.rs` の `Graph` 構造体（`query` フィールドの後）に追加:

```rust
    /// series 名（下層の生ラベル）→ UI 表示名の置換マップ（任意）。
    /// 下層固有の series 名の知識を config に留める（設計原則 2 と同型）。
    /// マップに無い series は素通し。検証はしない — 知らないキーは単に使われない。
    #[serde(default)]
    pub series_labels: Option<std::collections::HashMap<String, String>>,
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test --lib config`（または `cargo test graph_series_labels_parses`）
Expected: PASS（既存 config テストも全緑）

- [ ] **Step 5: config.example.toml に例を追記**

既存のコメント例（`# [[graph]]` の co2 ブロックの後）に追加:

```toml
#
# [[graph]]
# name  = "machine"
# label = "jarvis"
# unit  = "%"      # 温度だけ series_labels 側で ℃ を明示
# query = ["embalse-query", "machine", "{period}"]
# [graph.series_labels]  # series 名 → UI 表示名（任意。無いキーは素通し）
# cpu_used_pct  = "CPU (%)"
# mem_used_pct  = "メモリ (%)"
# disk_used_pct = "ディスク (%)"
# cpu_temp_c    = "温度 (℃)"
```

- [ ] **Step 6: コミット**

```bash
git add src/config.rs config.example.toml
git commit -m "feat: graph に series_labels マップを追加（series 名の表示名置換）"
```

---

### Task 2: normalize — `normalize_graph_rows` に series_labels 適用

**Files:**
- Modify: `src/normalize.rs:127-158`（normalize_graph_rows）+ tests の既存呼び出し
- Modify: `src/main.rs:663`（呼び出し側）+ tests の test_app config
- Test: `src/normalize.rs` / `src/main.rs`

**Interfaces:**
- Consumes: Task 1 の `Graph.series_labels`
- Produces: `normalize_graph_rows(rows: &[Value], default_label: &str, series_labels: Option<&std::collections::HashMap<String, String>>) -> Vec<GraphSeries>`（第 3 引数が新規。既存挙動は `None` で不変）

- [ ] **Step 1: 失敗するテストを書く**

`src/normalize.rs` tests に追加（`graph_rows_empty_is_empty` の後）:

```rust
    #[test]
    fn graph_rows_series_labels_mapped() {
        let mut m = std::collections::HashMap::new();
        m.insert("cpu_used_pct".to_string(), "CPU (%)".to_string());
        let rows = [
            json!({"ts": "t1", "series": "cpu_used_pct", "value": 12.0}),
            json!({"ts": "t1", "series": "mem_used_pct", "value": 45.0}),
        ];
        let s = normalize_graph_rows(&rows, "jarvis", Some(&m));
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].label, "CPU (%)"); // マップにあれば置換
        assert_eq!(s[1].label, "mem_used_pct"); // 無ければ素通し
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test graph_rows_series_labels_mapped`
Expected: コンパイルエラー（引数が 2 個）

- [ ] **Step 3: 実装 + 既存呼び出しの更新**

`src/normalize.rs` の `normalize_graph_rows` を差し替え（doc コメントの末尾 1 行と signature、label 解決部のみ変更）:

```rust
/// embalse 読み出し CLI の契約 JSON（フラット行配列）→ チャート系列。
///
/// 契約: `[{"ts": "ISO8601", "series": "ラベル(任意)", "value": 数値}, ...]`
/// series 省略行は default_label（グラフの表示名）に束ねる。ts / value が
/// 欠けた・型不正の行は drop（部分的に壊れたデータで全体を落とさない）。
/// 系列は初出順、各系列内は ts 昇順（同一オフセットの ISO8601 は辞書順=時刻順）。
/// series_labels は series 名 → UI 表示名の置換マップ（無いキーは素通し）。
/// 下層（embalse）の出力形式に関する知識はこの関数に閉じる（設計原則 4）。
pub fn normalize_graph_rows(
    rows: &[Value],
    default_label: &str,
    series_labels: Option<&std::collections::HashMap<String, String>>,
) -> Vec<GraphSeries> {
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
        let raw = row
            .get("series")
            .and_then(Value::as_str)
            .unwrap_or(default_label);
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

既存テスト 5 箇所（`graph_rows_single_series_gets_default_label` / `graph_rows_grouped_by_series_in_first_appearance_order` / `graph_rows_sorted_by_ts_ascending` / `graph_rows_invalid_rows_dropped` / `graph_rows_empty_is_empty`）の呼び出しに第 3 引数 `None` を追加。例:

```rust
        let s = normalize_graph_rows(&rows, "太陽光発電", None);
```

`src/main.rs:663` の呼び出しを更新:

```rust
    let series =
        normalize::normalize_graph_rows(&rows, graph.label(), graph.series_labels.as_ref());
```

- [ ] **Step 4: API レベルのテストを追加**

`src/main.rs` tests の `test_app` config に graph を 1 本追加（`notarray` graph の後）:

```toml
            [[graph]]
            name  = "machine"
            label = "jarvis"
            unit  = "%"
            query = ["sh", "-c", "printf '[{\"ts\":\"t1\",\"series\":\"cpu_used_pct\",\"value\":12.3},{\"ts\":\"t1\",\"series\":\"cpu_temp_c\",\"value\":52.0}]'", "sh", "{period}"]
            series_labels = { cpu_used_pct = "CPU (%)", cpu_temp_c = "温度 (℃)" }
```

テストを追加（`graph_non_json_stdout_is_502` の後）:

```rust
    #[tokio::test]
    async fn graph_series_labels_applied() {
        let (st, v) = call("GET", "/api/graphs/machine").await;
        assert_eq!(st, StatusCode::OK);
        let labels: Vec<&str> = v["series"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["CPU (%)", "温度 (℃)"]);
    }
```

- [ ] **Step 5: 全テスト + clippy**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全緑・警告なし

- [ ] **Step 6: コミット**

```bash
git add src/normalize.rs src/main.rs
git commit -m "feat: グラフ正規化で series_labels を適用（machine の metric 名を表示名へ）"
```

---

### Task 3: config — `[health]` セクション

**Files:**
- Modify: `src/config.rs`（Config / 新 Health 構造体 / ConfigError / validate）
- Modify: `config.example.toml`
- Test: `src/config.rs`

**Interfaces:**
- Produces: `Config.health: Option<Health>`、`Health { label: Option<String>, command: Vec<String>, labels: Option<HashMap<String, String>> }`（Task 5 が使う）
- Produces: `ConfigError::EmptyHealthCommand`

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` tests に追加:

```rust
    #[test]
    fn health_parses() {
        let p = write_tmp(
            "healthok",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [health]
            label   = "jarvis"
            command = ["embalse-query", "health"]
            [health.labels]
            cpu_used_pct = "CPU"
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        let h = cfg.health.as_ref().unwrap();
        assert_eq!(h.label.as_deref(), Some("jarvis"));
        assert_eq!(h.command, vec!["embalse-query", "health"]);
        assert_eq!(h.labels.as_ref().unwrap().get("cpu_used_pct").unwrap(), "CPU");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn health_absent_is_none() {
        let p = write_tmp(
            "healthnone",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            "##,
        );
        let cfg = Config::load(&p).unwrap();
        assert!(cfg.health.is_none());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn health_empty_command_rejected() {
        let p = write_tmp(
            "healthempty",
            r##"
            [[device]]
            name = "s1"
            get_state = ["enl", "get", "x", "026301", "open_close_state"]
            open = ["enl", "set", "x", "026301", "open_close_operation", "open"]
            close = ["enl", "set", "x", "026301", "open_close_operation", "close"]
            [health]
            command = []
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::EmptyHealthCommand)
        ));
        std::fs::remove_file(p).ok();
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test health_`
Expected: コンパイルエラー（`health` フィールド / `EmptyHealthCommand` が存在しない）

- [ ] **Step 3: 最小実装**

`src/config.rs` の `Config` に追加（`graphs` の後）:

```rust
    #[serde(default)]
    pub health: Option<Health>,
```

`Graph` 定義の後に構造体を追加:

```rust
/// マシン健全性レポートのコマンド定義（任意）。未設定なら /api/health ごと無効。
/// しきい値判定は下層（embalse）の責務 — mando は exec して契約 JSON を
/// 正規化するだけで、metric 名もコマンド名も本体は知らない（設計原則 2）。
#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    /// バナー表示の対象名（任意。例 "jarvis"）。未指定なら UI は名前なしで出す。
    #[serde(default)]
    pub label: Option<String>,
    /// exec するコマンド配列。
    pub command: Vec<String>,
    /// metric 名 → UI 表示名の置換マップ（任意）。無いキーは metric 名を素通し。
    #[serde(default)]
    pub labels: Option<std::collections::HashMap<String, String>>,
}
```

`ConfigError` に variant 追加:

```rust
    EmptyHealthCommand,
```

Display 実装に追加:

```rust
            ConfigError::EmptyHealthCommand => write!(f, "health: command が空"),
```

`validate()` の graph 検証ループの後（`Ok(())` の直前）に追加:

```rust
        if let Some(h) = &self.health {
            if h.command.is_empty() {
                return Err(ConfigError::EmptyHealthCommand);
            }
        }
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全緑・警告なし

- [ ] **Step 5: config.example.toml に例を追記**

graph のコメント例の後に追加:

```toml

# ── マシン健全性（任意）─────────────────────────────
# 下層の health CLI（embalse-query health 等）を exec し、異常時のみ画面上部に
# バナーを出す。未設定ならこの機能ごと無効。しきい値判定は下層の責務。
#
# [health]
# label   = "jarvis"                       # バナーの対象名（任意）
# command = ["embalse-query", "health"]
# [health.labels]  # metric 名 → 表示名（任意。無いキーは素通し）
# cpu_used_pct  = "CPU"
# mem_used_pct  = "メモリ"
# disk_used_pct = "ディスク"
# cpu_temp_c    = "CPU温度"
```

- [ ] **Step 6: コミット**

```bash
git add src/config.rs config.example.toml
git commit -m "feat: config に [health] セクションを追加"
```

---

### Task 4: normalize — `normalize_health_rows`

**Files:**
- Modify: `src/normalize.rs`
- Test: `src/normalize.rs`

**Interfaces:**
- Produces: `normalize_health_rows(rows: &[Value], labels: Option<&std::collections::HashMap<String, String>>) -> HealthReport`
- Produces: `HealthReport { worst: String, items: Vec<HealthItem> }`、`HealthItem { label: String, value: Option<f64>, ts: Option<String>, level: String }`（value / ts は None なら JSON から省略）

- [ ] **Step 1: 失敗するテストを書く**

`src/normalize.rs` tests に追加:

```rust
    #[test]
    fn health_rows_normalized_with_labels() {
        let mut m = std::collections::HashMap::new();
        m.insert("disk_used_pct".to_string(), "ディスク".to_string());
        let rows = [
            json!({"metric": "disk_used_pct", "value": 83.2, "ts": "t1", "level": "warn"}),
            json!({"metric": "cpu_temp_c", "value": 55.0, "ts": "t1", "level": "ok"}),
        ];
        let r = normalize_health_rows(&rows, Some(&m));
        assert_eq!(r.worst, "warn");
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.items[0].label, "ディスク"); // マップにあれば置換
        assert_eq!(r.items[0].value, Some(83.2));
        assert_eq!(r.items[0].level, "warn");
        assert_eq!(r.items[1].label, "cpu_temp_c"); // 無ければ素通し
    }

    #[test]
    fn health_worst_ranking_crit_over_stale_over_warn() {
        // crit > stale > warn > ok（stale = 収集停止も気づくべき異常）
        let rows = [
            json!({"metric": "a", "level": "warn", "value": 1.0, "ts": "t"}),
            json!({"metric": "b", "level": "stale"}),
        ];
        assert_eq!(normalize_health_rows(&rows, None).worst, "stale");
        let rows = [
            json!({"metric": "a", "level": "stale"}),
            json!({"metric": "b", "level": "crit", "value": 99.0, "ts": "t"}),
        ];
        assert_eq!(normalize_health_rows(&rows, None).worst, "crit");
        let rows = [json!({"metric": "a", "level": "ok", "value": 1.0, "ts": "t"})];
        assert_eq!(normalize_health_rows(&rows, None).worst, "ok");
    }

    #[test]
    fn health_stale_row_has_no_value_ts() {
        let rows = [json!({"metric": "mem_used_pct", "level": "stale"})];
        let r = normalize_health_rows(&rows, None);
        assert_eq!(r.items[0].value, None);
        assert_eq!(r.items[0].ts, None);
        assert_eq!(r.items[0].level, "stale");
    }

    #[test]
    fn health_invalid_rows_dropped() {
        let rows = [
            json!({"metric": "a", "level": "banana"}), // 未知 level
            json!({"level": "ok"}),                    // metric 欠落
            json!("garbage"),                          // オブジェクトですらない
            json!({"metric": "d", "level": "ok", "value": 1.0, "ts": "t"}),
        ];
        let r = normalize_health_rows(&rows, None);
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].label, "d");
    }

    #[test]
    fn health_empty_is_stale() {
        // 判定材料ゼロ＝収集停止と同じ扱い（ok と偽らない）。
        let r = normalize_health_rows(&[], None);
        assert!(r.items.is_empty());
        assert_eq!(r.worst, "stale");
        let rows = [json!({"metric": "a", "level": "banana"})];
        assert_eq!(normalize_health_rows(&rows, None).worst, "stale");
    }

    #[test]
    fn health_item_serializes_without_none_fields() {
        let r = normalize_health_rows(&[json!({"metric": "a", "level": "stale"})], None);
        assert_eq!(
            serde_json::to_string(&r.items[0]).unwrap(),
            r#"{"label":"a","level":"stale"}"#
        );
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test health_`
Expected: コンパイルエラー（`normalize_health_rows` が存在しない）

- [ ] **Step 3: 実装**

`src/normalize.rs` の `normalize_graph_rows` の後に追加:

```rust
/// health 1 項目。契約行の metric を UI 表示名に写したもの。
#[derive(Debug, PartialEq, Serialize)]
pub struct HealthItem {
    pub label: String,
    /// stale 行は value / ts を持たない（契約どおり省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    pub level: String,
}

/// health レポート。worst は全項目の最悪 level。
#[derive(Debug, PartialEq, Serialize)]
pub struct HealthReport {
    pub worst: String,
    pub items: Vec<HealthItem>,
}

/// level の深刻度順位（インデックス = 深刻度）。stale（収集停止）は warn より上 —
/// 「観測できていない」こと自体が気づくべき異常（crit ほどの緊急ではない）。
const HEALTH_LEVELS: [&str; 4] = ["ok", "warn", "stale", "crit"];

/// 下層 health CLI の契約 JSON（`[{"metric", "value"?, "ts"?, "level"}]`）→ UI 向けレポート。
/// しきい値判定は下層の責務 — ここでは level を写すだけで判定しない。
/// level が 4 値以外・metric 欠落の行は drop（解釈できないものを ok と偽らない）。
/// items が空（0 行 / 全行 drop）なら worst = "stale"（判定材料ゼロ＝収集停止扱い）。
/// 下層（embalse）の出力形式に関する知識はこの関数に閉じる（設計原則 4）。
pub fn normalize_health_rows(
    rows: &[Value],
    labels: Option<&std::collections::HashMap<String, String>>,
) -> HealthReport {
    let mut items = Vec::new();
    let mut worst = 0;
    for row in rows {
        let Some(metric) = row.get("metric").and_then(Value::as_str) else {
            continue;
        };
        let Some(level) = row.get("level").and_then(Value::as_str) else {
            continue;
        };
        let Some(rank) = HEALTH_LEVELS.iter().position(|l| *l == level) else {
            continue;
        };
        let label = labels
            .and_then(|m| m.get(metric))
            .map(String::as_str)
            .unwrap_or(metric);
        worst = worst.max(rank);
        items.push(HealthItem {
            label: label.to_string(),
            value: row.get("value").and_then(Value::as_f64),
            ts: row.get("ts").and_then(Value::as_str).map(str::to_string),
            level: level.to_string(),
        });
    }
    let worst = if items.is_empty() {
        "stale".to_string()
    } else {
        HEALTH_LEVELS[worst].to_string()
    };
    HealthReport { worst, items }
}
```

- [ ] **Step 4: テスト + clippy**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全緑・警告なし

- [ ] **Step 5: コミット**

```bash
git add src/normalize.rs
git commit -m "feat: health 契約 JSON の正規化（worst 順位 crit>stale>warn>ok）"
```

---

### Task 5: API — `GET /api/health`

**Files:**
- Modify: `src/main.rs`（route / handler / CLAUDE.md 記載の API 一覧）
- Modify: `CLAUDE.md`（安定ミニ API の一覧に 1 行）
- Test: `src/main.rs`

**Interfaces:**
- Consumes: Task 3 の `Config.health`、Task 4 の `normalize_health_rows` / `HealthReport`
- Produces: `GET /api/health` → 200 `{"label"?, "worst", "items": [...]}` / 404（未設定）/ 502（exec 失敗・契約違反）

- [ ] **Step 1: 失敗するテストを書く**

`src/main.rs` tests: まず `test_app` の config 末尾（machine graph の後）に追加:

```toml
            [health]
            label   = "jarvis"
            command = ["sh", "-c", "printf '[{\"metric\":\"cpu_used_pct\",\"value\":12.3,\"ts\":\"t1\",\"level\":\"ok\"},{\"metric\":\"disk_used_pct\",\"value\":83.2,\"ts\":\"t1\",\"level\":\"warn\"}]'"]
            [health.labels]
            cpu_used_pct  = "CPU"
            disk_used_pct = "ディスク"
```

`call` ヘルパの後に、任意 config で叩くヘルパを追加:

```rust
    async fn call_on(cfg_toml: &str, method: &str, path: &str) -> (axum::http::StatusCode, Value) {
        let app = Arc::new(App {
            config: toml::from_str(cfg_toml).unwrap(),
            executor: Executor::new(),
            graph_executor: Executor::new(),
        });
        let res = router(app)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }
```

テストを追加:

```rust
    #[tokio::test]
    async fn health_normalized_with_labels() {
        let (st, v) = call("GET", "/api/health").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["label"], "jarvis");
        assert_eq!(v["worst"], "warn");
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["label"], "CPU");
        assert_eq!(items[1]["label"], "ディスク");
        assert_eq!(items[1]["value"], 83.2);
        assert_eq!(items[1]["level"], "warn");
    }

    const MINIMAL_DEVICE: &str = r##"
        [[device]]
        name = "s"
        get_state = ["sh", "-c", "printf '{}'"]
        open  = ["sh", "-c", "printf '{}'"]
        close = ["sh", "-c", "printf '{}'"]
    "##;

    #[tokio::test]
    async fn health_not_configured_is_404() {
        let (st, _) = call_on(MINIMAL_DEVICE, "GET", "/api/health").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn health_exec_failure_is_502() {
        let cfg = format!(
            "{MINIMAL_DEVICE}\n[health]\ncommand = [\"sh\", \"-c\", \"exit 1\"]\n"
        );
        let (st, v) = call_on(&cfg, "GET", "/api/health").await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
        assert_eq!(v["error"], "health query failed");
    }

    #[tokio::test]
    async fn health_non_json_stdout_is_502() {
        let cfg = format!(
            "{MINIMAL_DEVICE}\n[health]\ncommand = [\"sh\", \"-c\", \"printf 'not-json'\"]\n"
        );
        let (st, _) = call_on(&cfg, "GET", "/api/health").await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test health_normalized_with_labels`
Expected: FAIL（404 が返る — route が無い）

- [ ] **Step 3: 実装**

`src/main.rs` の `router()` に追加（`/api/graphs/:name` の後）:

```rust
        .route("/api/health", get(get_health))
```

`graph_unavailable` の後に追加:

```rust
#[derive(Serialize)]
struct HealthView {
    /// バナー表示の対象名（config の health.label。未指定なら省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(flatten)]
    report: normalize::HealthReport,
}

/// マシン健全性レポート。health テンプレを exec し契約 JSON を正規化して返す。
/// しきい値判定は下層（embalse）の責務 — mando は判定しない。
/// exec はグラフ用 Executor に相乗り（3610 と無関係な読み系。devices の枠に入れない）。
async fn get_health(State(app): State<Shared>) -> Response {
    let Some(health) = &app.config.health else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"health not configured"}"#.to_string(),
        )
            .into_response();
    };
    let result = run_graph_cmd(&app.graph_executor, &health.command, GRAPH_QUERY_TIMEOUT).await;
    if result.outcome != ExecOutcome::Success {
        tracing::warn!(
            outcome = ?result.outcome,
            stderr = %result.stderr.trim(),
            "health query 非成功"
        );
        return health_unavailable();
    }
    let rows = match serde_json::from_str::<Value>(&result.stdout) {
        Ok(Value::Array(rows)) => rows,
        Ok(_) | Err(_) => {
            tracing::warn!("health query の stdout が契約 JSON 配列でない");
            return health_unavailable();
        }
    };
    Json(HealthView {
        label: health.label.clone(),
        report: normalize::normalize_health_rows(&rows, health.labels.as_ref()),
    })
    .into_response()
}

/// 下層の health 読み出しに失敗（exec 非成功・契約 JSON でない）→ 502。
fn health_unavailable() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"health query failed"}"#.to_string(),
    )
        .into_response()
}
```

- [ ] **Step 4: テスト + clippy**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全緑・警告なし

- [ ] **Step 5: CLAUDE.md の API 一覧を更新**

`CLAUDE.md` の「やること（安定ミニ API）」リスト末尾に追加:

```markdown
- `GET  /api/health` — health テンプレを exec → 正規化 `{ "label"?, "worst": "ok|warn|crit|stale", "items": [...] }`（`[health]` 未設定なら 404）
```

- [ ] **Step 6: コミット**

```bash
git add src/main.rs CLAUDE.md
git commit -m "feat: GET /api/health（health テンプレ exec + 正規化）"
```

---

### Task 6: UI — 異常時のみの health バナー

**Files:**
- Modify: `index.html`（CSS / body 直下の要素 / JS）

**Interfaces:**
- Consumes: Task 5 の `GET /api/health`

- [ ] **Step 1: CSS を追加**

`index.html` の `.boot` スタイルの前に追加:

```css
  /* ── マシン健全性バナー（異常時のみ表示）─────────── */
  #hbanner { max-width: 560px; margin: 0 auto; padding: 6px 14px 0; }
  .hbanner {
    padding: 10px 14px; border-radius: 12px; border: 1px solid;
    font-size: 13px; font-weight: 600; line-height: 1.5;
    cursor: pointer; touch-action: manipulation; user-select: none;
  }
  .hbanner.warn  { background: rgba(232,161,58,.12);  border-color: rgba(232,161,58,.4);  color: var(--closed); }
  .hbanner.crit  { background: rgba(240,99,90,.12);   border-color: rgba(240,99,90,.45);  color: var(--warn); }
  .hbanner.stale { background: rgba(139,147,167,.10); border-color: rgba(139,147,167,.35); color: var(--muted); }
```

- [ ] **Step 2: バナー要素を追加**

`</header>` と `<main id="app">` の間に追加:

```html
<div id="hbanner" hidden></div>
```

- [ ] **Step 3: JS を追加**

`/* ── きろく（embalse グラフ）── */` ブロックの前に追加:

```js
/* ── マシン健全性バナー（異常時のみ表示）────────────── */
const HEALTH_LEVEL_MSG = { warn: "注意", crit: "危険", stale: "収集停止" };

// boot 時に 1 回だけ取得。ポーリングしない（マシン状態は 5 分粒度でしか動かない）。
// 取得失敗・404（機能無効）は黙って出さない — 監視の失敗は操作の失敗ではない。
async function fetchHealth() {
  let report;
  try {
    report = await api("GET", "/api/health");
  } catch (e) {
    console.error("health 取得失敗", e);
    return;
  }
  if (report.worst === "ok") return;
  const bad = report.items.filter((i) => i.level !== "ok");
  const parts = bad.map((i) => {
    const v = i.value != null ? ` ${fmtNum(i.value)}` : "";
    return `${i.label}${v} (${HEALTH_LEVEL_MSG[i.level] || i.level})`;
  });
  const prefix = report.label ? `${report.label}: ` : "";
  const box = document.getElementById("hbanner");
  const el = document.createElement("div");
  el.className = `hbanner ${report.worst}`;
  el.textContent = `⚠ ${prefix}${parts.join(" / ")}`;
  // タップで閉じる（この表示中のみ。次回ロードで再判定）。
  el.addEventListener("click", () => { box.hidden = true; box.innerHTML = ""; });
  box.innerHTML = "";
  box.appendChild(el);
  box.hidden = false;
}
```

`boot()` 末尾の `refreshOnce();` の直前に追加:

```js
  fetchHealth();
```

- [ ] **Step 4: ビルド + 動作確認（スタブ config）**

セッションのスクラッチパッドディレクトリ（リポジトリ外の一時領域。以下 `$SCRATCH` と表記）に `health-stub.toml` を作成:

```toml
bind = "127.0.0.1:8899"
[[device]]
name = "shutter"
label = "テスト"
get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
open  = ["sh", "-c", "printf '{}'"]
close = ["sh", "-c", "printf '{}'"]
[health]
label   = "jarvis"
command = ["sh", "-c", "printf '[{\"metric\":\"disk_used_pct\",\"value\":83.2,\"ts\":\"t\",\"level\":\"warn\"},{\"metric\":\"cpu_temp_c\",\"value\":55.0,\"ts\":\"t\",\"level\":\"ok\"},{\"metric\":\"mem_used_pct\",\"level\":\"stale\"}]'"]
[health.labels]
disk_used_pct = "ディスク"
mem_used_pct  = "メモリ"
cpu_temp_c    = "CPU温度"
```

Run:

```bash
cargo build
MANDO_CONFIG=$SCRATCH/health-stub.toml ./target/debug/mando &
sleep 1
curl -s http://127.0.0.1:8899/api/health
```

Expected: `{"label":"jarvis","worst":"stale","items":[...]}`（warn と stale の混在 → worst は stale）。
ブラウザ確認（可能なら headless chromium でスクリーンショット）: バナーに
`⚠ jarvis: ディスク 83.2 (注意) / メモリ (収集停止)` が stale 色で出ること。
確認後にプロセスを kill する。

- [ ] **Step 5: 全テスト + clippy**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全緑・警告なし

- [ ] **Step 6: コミット**

```bash
git add index.html
git commit -m "feat: 異常時のみ表示する health バナー（boot 時 1 回取得）"
```

---

### Task 7: jarvis デプロイ + 結合確認

**Files:**
- Modify: jarvis 上の `/etc/mando/config.toml`（リポジトリ外の実 config）

**Interfaces:**
- Consumes: 全タスクの成果（バイナリ）+ jarvis 配備済みの `embalse-query`（machine / health 対応済み）

> 注意: ローカル ssh agent が不安定な既知問題あり（1Password 再起動で復旧）。
> 失敗したらユーザーに ssh agent の状態確認を依頼する。実 config の編集内容は
> 適用前にユーザーへ提示して確認を取る。

- [ ] **Step 1: jarvis 上で embalse-query の口を確認**

```bash
ssh jarvis 'embalse-query machine today | head -c 200; echo; embalse-query health'
```

Expected: それぞれ契約 JSON 配列（health は 4 要素）。失敗するなら embalse 側が未配備 — ユーザーに報告して止まる。

- [ ] **Step 2: 実 config に追記**

jarvis の `/etc/mando/config.toml`（jarvis ユーザー所有・sudo 不要）に追記:

```toml
[[graph]]
name  = "machine"
label = "jarvis"
unit  = "%"
query = ["embalse-query", "machine", "{period}"]
[graph.series_labels]
cpu_used_pct  = "CPU (%)"
mem_used_pct  = "メモリ (%)"
disk_used_pct = "ディスク (%)"
cpu_temp_c    = "温度 (℃)"

[health]
label   = "jarvis"
command = ["embalse-query", "health"]
[health.labels]
cpu_used_pct  = "CPU"
mem_used_pct  = "メモリ"
disk_used_pct = "ディスク"
cpu_temp_c    = "CPU温度"
```

（既存 [[graph]] 群の後・ファイル末尾に追加。`[graph.series_labels]` は直前の `[[graph]]` に付く点に注意。）

- [ ] **Step 3: デプロイ（メモリの手順どおり）**

```bash
git push ssh://jarvis/home/jarvis/repository/mando main:refs/heads/deploy-incoming
ssh jarvis 'cd ~/repository/mando && git merge --ff-only deploy-incoming && git rev-parse HEAD'
# ↑ HEAD がローカル main と一致することを必ず確認（pull 黙殺事故の既知パターン）
ssh jarvis 'cd ~/repository/mando && export PATH=$HOME/.cargo/bin:$PATH && cargo build --release'
ssh jarvis 'sudo install -Dm755 ~/repository/mando/target/release/mando /usr/local/bin/mando && sudo systemctl restart mando'
```

- [ ] **Step 4: 結合確認**

```bash
ssh jarvis 'curl -s http://localhost:8080/api/health'
ssh jarvis 'curl -s "http://localhost:8080/api/graphs/machine?period=today" | head -c 300'
```

Expected: health は `{"label":"jarvis","worst":...}`（正常運転中なら worst=ok）、machine は series_labels 適用済みの系列（"CPU (%)" 等）。スマホ/ブラウザで「きろく」に jarvis グラフが出ること、（worst が ok なら）バナーが出ないことを確認。

- [ ] **Step 5: 完了報告**

デプロイ結果・確認した応答をユーザーに報告する（コミットはローカル main に済み。push はユーザー指示に従う）。
