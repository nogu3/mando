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
}
