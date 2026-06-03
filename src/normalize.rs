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
    Open,
    Closed,
    Unknown,
}

/// enl の get 出力 JSON を正規化する。
///
/// enl は `{ "properties": [ { "name": "open_close_state", "value": ... } ] }`
/// の形で返す。`value` から開閉を判定する。スキーマや値が想定外なら Unknown。
///
/// 値の表現は機種で振れる（ECHONET Lite open_close_state EDT 0x41=open / 0x42=closed が
/// 代表だが、enl が文字列 "open"/"closed" に正規化して返すこともある）。両対応する。
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
            // ECHONET Lite open_close_state: 0x41 = 全開, 0x42 = 全閉。
            Some(0x41) => State::Open,
            Some(0x42) => State::Closed,
            _ => State::Unknown,
        },
        // ネスト: { "open_close_state": "open" } のような形にも一応対応。
        Value::Object(o) => o
            .get("open_close_state")
            .map(classify)
            .unwrap_or(State::Unknown),
        _ => State::Unknown,
    }
}

fn classify_str(s: &str) -> State {
    match s.trim().to_ascii_lowercase().as_str() {
        "open" | "fully_open" | "0x41" | "41" => State::Open,
        "closed" | "close" | "fully_closed" | "0x42" | "42" => State::Closed,
        _ => State::Unknown,
    }
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
    fn unknown_on_garbage() {
        assert_eq!(normalize_enl_state(&json!({})), State::Unknown);
        assert_eq!(
            normalize_enl_state(&json!({"properties":[]})),
            State::Unknown
        );
        let raw = json!({"properties":[{"name":"open_close_state","value":"???"}]});
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }
}
