//! 下層固有知識を一点に閉じ込める層。
//!
//! enl の JSON（`properties[].value`）→「開 / 閉 / 不明」への正規化だけが
//! バックエンド固有。casa は出力スキーマが変わるので、移行時はここだけ
//! 差し替える（設計原則 4）。フロント・API はこの結果しか見ない。

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// 0x41 fully_open
    Open,
    /// 0x42 fully_closed
    Closed,
    /// 0x43 opening（開動作中）
    Opening,
    /// 0x44 closing（閉動作中）
    Closing,
    /// 0x45 stopped_midway（途中停止）
    Stopped,
    /// light 点灯（mat onoff value=true）。
    On,
    /// light 消灯（mat onoff value=false）。
    Off,
    /// スキーマ・値が想定外。
    Unknown,
}

/// enl の get 出力 JSON を正規化する。
///
/// enl の実出力例:
/// `{"eoj":"026301","esv":"GetRes",...,"properties":[
///    {"epc":"EA","name":"open_close_state","value":{"state":"fully_closed"}}]}`
/// → `value.state` から開閉を判定する。スキーマや値が想定外なら Unknown。
///
/// 値の表現は機種・バックエンドで振れるので幅広く受ける:
/// オブジェクト `{"state": "fully_closed"}`、文字列 "open"/"closed"、数値 EDT
/// (0x41=open / 0x42=closed) のいずれにも対応する。
pub fn normalize_enl_state(raw: &Value) -> State {
    let Some(props) = raw.get("properties").and_then(Value::as_array) else {
        return State::Unknown;
    };

    // open_close_state プロパティを優先で探す。無ければ最初のプロパティ。
    let prop = props
        .iter()
        .find(|p| {
            p.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n == "open_close_state")
        })
        .or_else(|| props.first());

    let Some(value) = prop.and_then(|p| p.get("value")) else {
        return State::Unknown;
    };

    classify(value)
}

fn classify(value: &Value) -> State {
    match value {
        Value::String(s) => classify_str(s),
        Value::Number(n) => match n.as_i64() {
            // ECHONET Lite open_close_state EDT。
            Some(0x41) => State::Open,
            Some(0x42) => State::Closed,
            Some(0x43) => State::Opening,
            Some(0x44) => State::Closing,
            Some(0x45) => State::Stopped,
            _ => State::Unknown,
        },
        // enl の実形式: value = { "state": "fully_closed" }。
        // 後方互換で "open_close_state" キーも見る。
        Value::Object(o) => o
            .get("state")
            .or_else(|| o.get("open_close_state"))
            .map(classify)
            .unwrap_or(State::Unknown),
        _ => State::Unknown,
    }
}

fn classify_str(s: &str) -> State {
    match s.trim().to_ascii_lowercase().as_str() {
        "open" | "fully_open" | "0x41" | "41" => State::Open,
        "closed" | "close" | "fully_closed" | "0x42" | "42" => State::Closed,
        "opening" | "0x43" | "43" => State::Opening,
        "closing" | "0x44" | "44" => State::Closing,
        "stopped" | "stopped_midway" | "0x45" | "45" => State::Stopped,
        _ => State::Unknown,
    }
}

/// mat read（onoff / on-off）の出力 JSON を正規化する。
///
/// mat の実出力例:
/// `{"timestamp":"...","node_id":5,"endpoint":1,"cluster":"onoff",
///   "attribute":"on-off","value":true}`
/// → `value` の bool で点灯/消灯を判定する。スキーマや値が想定外なら Unknown。
/// casa 移行時はこの関数の中身だけ差し替える（設計原則 4）。
pub fn normalize_mat_onoff(raw: &Value) -> State {
    match raw.get("value") {
        Some(Value::Bool(true)) => State::On,
        Some(Value::Bool(false)) => State::Off,
        _ => State::Unknown,
    }
}

/// グラフ 1 系列。契約 JSON の行を series 別に束ねたもの。
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

/// health 1 項目。契約行の metric を UI 表示名に写したもの。
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_value() {
        let raw = json!({"properties":[{"name":"open_close_state","value":"open"}]});
        assert_eq!(normalize_enl_state(&raw), State::Open);
        let raw = json!({"properties":[{"name":"open_close_state","value":"closed"}]});
        assert_eq!(normalize_enl_state(&raw), State::Closed);
    }

    #[test]
    fn numeric_edt() {
        let raw = json!({"properties":[{"name":"open_close_state","value":0x41}]});
        assert_eq!(normalize_enl_state(&raw), State::Open);
        let raw = json!({"properties":[{"name":"open_close_state","value":0x42}]});
        assert_eq!(normalize_enl_state(&raw), State::Closed);
    }

    #[test]
    fn hex_string() {
        let raw = json!({"properties":[{"name":"open_close_state","value":"0x42"}]});
        assert_eq!(normalize_enl_state(&raw), State::Closed);
    }

    #[test]
    fn picks_named_property_among_many() {
        let raw = json!({"properties":[
            {"name":"operation_status","value":"on"},
            {"name":"open_close_state","value":"open"}
        ]});
        assert_eq!(normalize_enl_state(&raw), State::Open);
    }

    #[test]
    fn real_enl_format() {
        // enl の実出力: value はオブジェクト {"state": "fully_closed"}。
        let raw = json!({
            "eoj":"026301","esv":"GetRes","ip":"192.168.1.222",
            "properties":[{"edt_hex":"42","epc":"EA","name":"open_close_state","pdc":1,
                           "value":{"state":"fully_closed"}}]
        });
        assert_eq!(normalize_enl_state(&raw), State::Closed);

        let raw =
            json!({"properties":[{"name":"open_close_state","value":{"state":"fully_open"}}]});
        assert_eq!(normalize_enl_state(&raw), State::Open);
    }

    #[test]
    fn all_five_states() {
        let cases = [
            ("fully_open", State::Open),
            ("fully_closed", State::Closed),
            ("opening", State::Opening),
            ("closing", State::Closing),
            ("stopped_midway", State::Stopped),
        ];
        for (s, want) in cases {
            let raw = json!({"properties":[{"name":"open_close_state","value":{"state":s}}]});
            assert_eq!(normalize_enl_state(&raw), want, "state={s}");
        }
    }

    #[test]
    fn numeric_edt_all() {
        for (edt, want) in [
            (0x41, State::Open),
            (0x42, State::Closed),
            (0x43, State::Opening),
            (0x44, State::Closing),
            (0x45, State::Stopped),
        ] {
            let raw = json!({"properties":[{"name":"open_close_state","value":edt}]});
            assert_eq!(normalize_enl_state(&raw), want, "edt={edt:#x}");
        }
    }

    #[test]
    fn unknown_on_garbage() {
        assert_eq!(normalize_enl_state(&json!({})), State::Unknown);
        assert_eq!(
            normalize_enl_state(&json!({"properties":[]})),
            State::Unknown
        );
        let raw = json!({"properties":[{"name":"open_close_state","value":"???"}]});
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }

    #[test]
    fn mat_onoff_real_format() {
        // mat read の実出力形式。
        let raw = json!({
            "timestamp": "2026-07-09T12:00:00+09:00",
            "node_id": 5, "endpoint": 1,
            "cluster": "onoff", "attribute": "on-off",
            "value": true
        });
        assert_eq!(normalize_mat_onoff(&raw), State::On);
        let raw = json!({"value": false});
        assert_eq!(normalize_mat_onoff(&raw), State::Off);
    }

    #[test]
    fn mat_onoff_garbage_is_unknown() {
        assert_eq!(normalize_mat_onoff(&json!({})), State::Unknown);
        assert_eq!(normalize_mat_onoff(&json!({"value": "on"})), State::Unknown);
        assert_eq!(normalize_mat_onoff(&json!({"value": 1})), State::Unknown);
        assert_eq!(normalize_mat_onoff(&json!(null)), State::Unknown);
    }

    #[test]
    fn on_off_serialize_snake_case() {
        assert_eq!(serde_json::to_string(&State::On).unwrap(), "\"on\"");
        assert_eq!(serde_json::to_string(&State::Off).unwrap(), "\"off\"");
    }

    #[test]
    fn graph_rows_single_series_gets_default_label() {
        let rows = [
            json!({"ts": "2026-07-15T10:00:00+09:00", "value": 100.0}),
            json!({"ts": "2026-07-15T10:05:00+09:00", "value": 200.0}),
        ];
        let s = normalize_graph_rows(&rows, "太陽光発電", None);
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
        let s = normalize_graph_rows(&rows, "CO2", None);
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
        let s = normalize_graph_rows(&rows, "x", None);
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
        let s = normalize_graph_rows(&rows, "x", None);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].points, vec![("t3".to_string(), 3.0)]);
    }

    #[test]
    fn graph_rows_empty_is_empty() {
        assert!(normalize_graph_rows(&[], "x", None).is_empty());
    }

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
}
