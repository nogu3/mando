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
    // casa get は enl 出力を {"value": {...}} でラップする。value の中に
    // properties があればそれを内側とみなし、無ければ raw 自身を使う。
    // これで casa 経由・enl 直の両方を受ける（設計原則4の一点変更）。
    let inner = raw
        .get("value")
        .filter(|v| v.get("properties").is_some())
        .unwrap_or(raw);
    let Some(props) = inner.get("properties").and_then(Value::as_array) else {
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
            Some(0x30) => State::On,
            Some(0x31) => State::Off,
            _ => State::Unknown,
        },
        // enl/casa の実形式: value = { "<prop>": "<値>" }。値オブジェクトのキーは
        // プロパティで振れる（shutter は "state"、casa の on/off は "power"）。
        // 後方互換で "open_close_state" キーも見る。
        Value::Object(o) => o
            .get("state")
            .or_else(|| o.get("open_close_state"))
            .or_else(|| o.get("power"))
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
        "on" | "0x30" | "30" => State::On,
        "off" | "0x31" | "31" => State::Off,
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
    raw.get("value")
        .map(normalize_onoff_value)
        .unwrap_or(State::Unknown)
}

/// `onoff/on-off` の値 → 論理 state。bool 以外は Unknown。
pub fn normalize_onoff_value(value: &Value) -> State {
    match value {
        Value::Bool(true) => State::On,
        Value::Bool(false) => State::Off,
        _ => State::Unknown,
    }
}

/// read の戻り値から node_id を取り出す（config とのドリフト検出用）。
pub fn read_node_id(raw: &Value) -> Option<u64> {
    raw.get("node_id").and_then(Value::as_u64)
}

/// グラフ 1 系列。契約 JSON の行を series 別に束ねたもの。
#[derive(Debug, PartialEq, Serialize)]
pub struct GraphSeries {
    pub label: String,
    /// (ts, value)。ts 昇順。JSON では [["ts", value], ...] になる。
    pub points: Vec<(String, f64)>,
}

/// グラフ正規化の結果。通常系列と、見出し併記用の sub 要約値（@summary 行）。
#[derive(Debug, PartialEq, Serialize)]
pub struct GraphNormalized {
    pub series: Vec<GraphSeries>,
    pub summary: Option<f64>,
}

/// embalse 読み出し CLI の契約 JSON（フラット行配列）→ チャート系列。
///
/// 契約: `[{"ts": "ISO8601", "series": "ラベル(任意)", "value": 数値}, ...]`
/// series 省略行は default_label（グラフの表示名）に束ねる。ts / value が
/// 欠けた・型不正の行は drop（部分的に壊れたデータで全体を落とさない）。
/// 系列は初出順、各系列内は ts 昇順（同一オフセットの ISO8601 は辞書順=時刻順）。
/// series_labels は series 名 → UI 表示名の置換マップ（無いキーは素通し）。
/// `@summary` 行は sub 要約値として抽出し series から除外（ts 最大を採用）。
/// 下層（embalse）の出力形式に関する知識はこの関数に閉じる（設計原則 4）。
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
            if summary_ts.as_deref().is_none_or(|t| ts > t) {
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

/// メッシュの network セクション。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshNetwork {
    pub name: Option<String>,
    pub channel: Option<u64>,
    pub leader_router_id: Option<u64>,
    /// partition_ids が 2 つ以上 = メッシュが分裂している。
    pub split: bool,
}

/// メッシュの 1 頂点。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshNode {
    /// mat の安定キー（ext:… / node:… / rloc:…）をそのまま。
    pub id: String,
    /// UI 表示名。
    pub name: String,
    /// "ours"（自 fabric）| "named"（他人だが名前あり）| "anonymous"。
    pub kind: String,
    pub role: String,
    pub is_leader: bool,
    /// 自 fabric ノードのみが持つ。他人の参加者は probe していないので None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
}

/// メッシュの 1 エッジ。lqi / rssi / fer は「悪いほうの視点」の値。
/// mat 側でフィールドが省略されうるため、欠けた実測値をゼロ埋めせず
/// Option のまま伝える（原則7: 未測定を測定値として偽らない）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshEdge {
    pub a: String,
    pub b: String,
    /// "good" | "fair" | "weak" | "bad" | "route_only"。
    /// "route_only" は「選ばれた視点に lqi が無く、かつ fer も無いか
    /// fer_weak 未満」の場合（`grade_link`）。fer が閾値未満で読めている
    /// ケースでは fer 自体はこの edge の出力に残る — route_only だからと
    /// いって品質値が一切読めないとは限らない（honest な描き分けは
    /// UI 上「薄い破線」）。
    pub grade: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lqi: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fer: Option<u8>,
}

/// フロントが見る安定形。mat の生 JSON はここから先に出さない。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshView {
    pub network: MeshNetwork,
    pub nodes: Vec<MeshNode>,
    pub edges: Vec<MeshEdge>,
    /// 自 fabric なのに自分の ext を名乗れなかったノード数。
    /// 二重出現の注記を出すかの判断に使う（mat#13）。
    pub unidentified_count: usize,
    /// mat が付けた収集時刻。
    pub fetched_at: Option<String>,
}

/// 1 視点ぶんの neighbor 実測値。mat 側の `LinkMetrics` は全フィールドが
/// `Option`（`skip_serializing_if`）なので、ここも Option で受ける。
/// `{"lqi": 3}`（rssi/fer 欠落）や `{}` すら実際の出力形。
#[derive(Clone, Copy)]
struct MeshLink {
    lqi: Option<u8>,
    rssi: Option<i64>,
    fer: Option<u8>,
}

fn read_link(v: &Value) -> Option<MeshLink> {
    let o = v.as_object()?;
    Some(MeshLink {
        lqi: o.get("lqi").and_then(Value::as_u64).map(|n| n as u8),
        rssi: o.get("avg_rssi").and_then(Value::as_i64),
        fer: o.get("frame_error_rate").and_then(Value::as_u64).map(|n| n as u8),
    })
}

/// 等級の深刻度（大きいほど悪い）。
fn severity(grade: &str) -> u8 {
    match grade {
        "good" => 0,
        "fair" => 1,
        "weak" => 2,
        "bad" => 3,
        _ => 0,
    }
}

/// 両視点のうち「悪いほう」を選ぶ。ゼロ埋めせず、実際に値がある視点だけを
/// 比較材料にする:
/// 1. lqi を持つ視点があれば、その中で lqi 最小（同値なら fer 最大。
///    fer 欠落はタイで実値持ちに負ける）。
/// 2. lqi を持つ視点が無ければ、fer を持つ視点の中で fer 最大。
/// 3. どちらも無ければ品質測定なし（呼び出し側で route_only にする）。
fn pick_worst(views: &[MeshLink]) -> Option<MeshLink> {
    let mut worst: Option<MeshLink> = None;
    for &v in views.iter().filter(|v| v.lqi.is_some()) {
        worst = Some(match worst {
            None => v,
            Some(cur) if is_worse_by_lqi(v, cur) => v,
            Some(cur) => cur,
        });
    }
    if worst.is_some() {
        return worst;
    }
    for &v in views.iter().filter(|v| v.fer.is_some()) {
        worst = Some(match worst {
            None => v,
            Some(cur) if v.fer > cur.fer => v,
            Some(cur) => cur,
        });
    }
    worst
}

/// candidate が current より悪いか（lqi 昇順で悪い＝小さいほう。
/// 同値なら fer 降順で悪い＝大きいほう。fer 欠落はタイで実値持ちに負ける）。
fn is_worse_by_lqi(candidate: MeshLink, current: MeshLink) -> bool {
    match candidate.lqi.cmp(&current.lqi) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => match (candidate.fer, current.fer) {
            (Some(a), Some(b)) => a > b,
            (Some(_), None) => true,
            (None, _) => false,
        },
    }
}

/// LQI があれば LQI から等級を出し、frame error rate が高ければ weak 以下へ
/// 落とす（FER は悪くする方向にしか働かない。lqi 0 の bad を weak に引き上げ
/// ない）。LQI が無ければ FER のみで判定し、それも無ければ「品質を読めない」
/// ことを正直に route_only として返す（原則7: 0 を捏造しない）。
fn grade_link(l: MeshLink, t: &crate::config::MeshThresholds) -> &'static str {
    let worsen_by_fer = l.fer.is_some_and(|f| f >= t.fer_weak);
    match l.lqi {
        Some(0) => "bad",
        Some(lqi) => {
            let by_lqi = if lqi <= t.lqi_weak {
                "weak"
            } else if lqi <= t.lqi_fair {
                "fair"
            } else {
                "good"
            };
            if worsen_by_fer && severity(by_lqi) < severity("weak") {
                "weak"
            } else {
                by_lqi
            }
        }
        None if worsen_by_fer => "weak",
        None => "route_only",
    }
}

/// `mat diag mesh` の出力 JSON → フロント向けの安定形。
///
/// mat 固有の知識（`a_sees_b` / `b_sees_a` の構造、`ext:` / `node:` の ID 規約、
/// LQI が 0–3 スケールであること）はこの関数に閉じる（設計原則 4）。
///
/// エッジ品質は両視点のうち**悪いほう**を採る（弱点探しが目的なので楽観側を
/// 採らない）。悪さの比較は LQI 昇順、同値なら誤り率降順（`pick_worst` 参照）。
/// mat 側のリンク実測値は各フィールドが省略されうるため、欠けた値を 0 として
/// 埋めることはしない（原則7）。lqi が読めず、かつ fer も読めないか fer_weak
/// 未満の視点しか無い場合は "route_only" として扱う（`grade_link`）。後者では
/// fer 自体はこの edge の出力に残る — route_only だからといって品質値が一切
/// 読めないとは限らない。隣接視点そのものが無い場合も同じ扱いになり、
/// どちらも UI 上は「接続しているが品質不明」を表す薄い破線として描かれる、
/// honest な描き分け。
pub fn normalize_mesh(
    raw: &Value,
    thresholds: &crate::config::MeshThresholds,
    labels: Option<&std::collections::HashMap<String, String>>,
) -> MeshView {
    let net = raw.get("network");
    let partitions = net
        .and_then(|n| n.get("partition_ids"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let network = MeshNetwork {
        name: net
            .and_then(|n| n.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        channel: net.and_then(|n| n.get("channel")).and_then(Value::as_u64),
        leader_router_id: net
            .and_then(|n| n.get("leader_router_id"))
            .and_then(Value::as_u64),
        split: partitions > 1,
    };

    let raw_nodes = raw
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut nodes = Vec::new();
    let mut unidentified_count = 0;
    for n in &raw_nodes {
        let Some(id) = n.get("id").and_then(Value::as_str) else {
            continue;
        };
        let alias = n.get("alias").and_then(Value::as_str);
        let label = n.get("label").and_then(Value::as_str);
        let (kind, name) = match (alias, label) {
            (Some(a), _) => {
                let shown = labels
                    .and_then(|m| m.get(a))
                    .map(String::as_str)
                    .unwrap_or(a);
                ("ours", shown.to_string())
            }
            (None, Some(l)) => ("named", l.to_string()),
            (None, None) => {
                let ext = n.get("ext_address").and_then(Value::as_str);
                let shown = match ext {
                    Some(e) if e.len() >= 4 => e[..4].to_string(),
                    Some(e) => e.to_string(),
                    None => id.to_string(),
                };
                ("anonymous", shown)
            }
        };
        // 自 fabric なのに ext を名乗れていない = mat#13 の二重出現候補。
        if alias.is_some() && id.starts_with("node:") {
            unidentified_count += 1;
        }
        let role = n
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        nodes.push(MeshNode {
            id: id.to_string(),
            name,
            kind: kind.to_string(),
            is_leader: role == "leader",
            role,
            probed: n.get("probed").and_then(Value::as_bool),
            error_kind: n
                .get("probe_error")
                .and_then(|e| e.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }

    let known: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut edges = Vec::new();
    for e in raw
        .get("edges")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let (Some(a), Some(b)) = (
            e.get("a").and_then(Value::as_str),
            e.get("b").and_then(Value::as_str),
        ) else {
            continue;
        };
        if !known.contains(a) || !known.contains(b) {
            continue;
        }
        let views: Vec<MeshLink> = ["a_sees_b", "b_sees_a"]
            .iter()
            .filter_map(|k| e.get(*k))
            .filter_map(read_link)
            .collect();
        // 悪いほう = LQI 昇順、同値なら誤り率降順（欠けた値はゼロ埋めしない）。
        let worst = pick_worst(&views);
        let edge = match worst {
            Some(l) => MeshEdge {
                a: a.to_string(),
                b: b.to_string(),
                grade: grade_link(l, thresholds).to_string(),
                lqi: l.lqi,
                rssi: l.rssi,
                fer: l.fer,
            },
            None => MeshEdge {
                a: a.to_string(),
                b: b.to_string(),
                grade: "route_only".to_string(),
                lqi: None,
                rssi: None,
                fer: None,
            },
        };
        edges.push(edge);
    }

    MeshView {
        network,
        nodes,
        edges,
        unidentified_count,
        fetched_at: raw
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn onoff_value_maps_bool_only() {
        assert_eq!(normalize_onoff_value(&json!(true)), State::On);
        assert_eq!(normalize_onoff_value(&json!(false)), State::Off);
        // bool 以外は解釈しない（0 / 1 を on/off と決めつけない）。
        assert_eq!(normalize_onoff_value(&json!(1)), State::Unknown);
        assert_eq!(normalize_onoff_value(&json!("on")), State::Unknown);
        assert_eq!(normalize_onoff_value(&json!(null)), State::Unknown);
    }

    #[test]
    fn reads_node_id_for_drift_check() {
        assert_eq!(
            read_node_id(&json!({"node_id": 6, "value": true})),
            Some(6)
        );
        assert_eq!(read_node_id(&json!({"value": true})), None);
        assert_eq!(read_node_id(&json!({"node_id": "6"})), None);
    }

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
    fn casa_envelope_closed() {
        // casa get の出力は enl 出力を "value" でラップする。
        let raw = json!({
            "device": "shutter1", "protocol": "echonet",
            "timestamp": "2026-07-18T21:00:00+09:00",
            "value": {
                "eoj": "026301", "esv": "GetRes", "ip": "192.168.1.222",
                "properties": [{"edt_hex":"42","epc":"EA","name":"open_close_state",
                                "pdc":1,"value":{"state":"fully_closed"}}]
            }
        });
        assert_eq!(normalize_enl_state(&raw), State::Closed);
    }

    #[test]
    fn casa_envelope_all_five_states() {
        let cases = [
            ("fully_open", State::Open),
            ("fully_closed", State::Closed),
            ("opening", State::Opening),
            ("closing", State::Closing),
            ("stopped_midway", State::Stopped),
        ];
        for (s, want) in cases {
            let raw = json!({
                "device": "shutter1", "protocol": "echonet",
                "value": {"properties":[{"name":"open_close_state","value":{"state":s}}]}
            });
            assert_eq!(normalize_enl_state(&raw), want, "state={s}");
        }
    }

    #[test]
    fn casa_envelope_unknown_property_is_unknown() {
        // value 内に properties はあるが、状態プロパティ（open_close_state / power）
        // でも既知の値でもない → Unknown。エンベロープを剥がした先で誤分類しない番犬。
        // （注: 照明の power は switch 用に on/off へ分類する。switch_casa_power_envelope 参照）
        let raw = json!({
            "device": "sensor", "protocol": "echonet",
            "value": {"eoj":"001101","esv":"GetRes",
                      "properties":[{"epc":"E0","name":"illuminance","value":{"illuminance":"bright"}}]}
        });
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }

    #[test]
    fn casa_envelope_value_without_properties_is_unknown() {
        // value はあるが properties キーを持たない → filter が外れて raw に戻り、
        // raw にもトップレベル properties が無いので Unknown。フォールバック分岐の番犬。
        let raw = json!({
            "device": "porch_light", "protocol": "echonet",
            "value": {"power": "on"}
        });
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }

    #[test]
    fn switch_on_off_object() {
        // enl の実出力: value はオブジェクト {"state": "on"}。
        let raw = json!({"properties":[
            {"epc":"80","name":"operation_status","value":{"state":"on"}}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[
            {"epc":"80","name":"operation_status","value":{"state":"off"}}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
    }

    #[test]
    fn switch_on_off_string() {
        let raw = json!({"properties":[{"name":"operation_status","value":"on"}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[{"name":"operation_status","value":"off"}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
        let raw = json!({"properties":[{"name":"operation_status","value":"0x30"}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[{"name":"operation_status","value":"31"}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
    }

    #[test]
    fn switch_on_off_numeric_edt() {
        // ECHONET Lite operation_status EDT: 0x30=ON / 0x31=OFF。
        let raw = json!({"properties":[{"name":"operation_status","value":0x30}]});
        assert_eq!(normalize_enl_state(&raw), State::On);
        let raw = json!({"properties":[{"name":"operation_status","value":0x31}]});
        assert_eq!(normalize_enl_state(&raw), State::Off);
    }

    #[test]
    fn switch_on_off_casa_envelope() {
        let raw = json!({
            "device": "fan", "protocol": "echonet",
            "value": {"properties":[
                {"name":"operation_status","value":{"state":"on"}}]}
        });
        assert_eq!(normalize_enl_state(&raw), State::On);
    }

    #[test]
    fn switch_unknown_on_garbage_value() {
        let raw = json!({"properties":[{"name":"operation_status","value":"heating"}]});
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }

    #[test]
    fn switch_casa_power_envelope() {
        // casa get <light> power の実出力: value オブジェクトのキーは "power"
        // （プロパティ名 power に対応）。shutter の "state" とは別キーなので、
        // classify のオブジェクト分岐が "power" も見る必要がある。
        let raw = json!({
            "device": "kitchen_light", "protocol": "echonet",
            "value": {"eoj":"029108","esv":"GetRes","ip":"192.168.1.22",
                "properties":[{"edt_hex":"31","epc":"80","name":"power","pdc":1,
                               "value":{"power":"off"}}]}
        });
        assert_eq!(normalize_enl_state(&raw), State::Off);

        let raw = json!({
            "device": "kitchen_light", "protocol": "echonet",
            "value": {"properties":[{"epc":"80","name":"power","value":{"power":"on"}}]}
        });
        assert_eq!(normalize_enl_state(&raw), State::On);
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
        let s = normalize_graph_rows(&rows, "太陽光発電", None).series;
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
        let s = normalize_graph_rows(&rows, "CO2", None).series;
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
        let s = normalize_graph_rows(&rows, "x", None).series;
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
        let s = normalize_graph_rows(&rows, "x", None).series;
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].points, vec![("t3".to_string(), 3.0)]);
    }

    #[test]
    fn graph_rows_empty_is_empty() {
        assert!(normalize_graph_rows(&[], "x", None).series.is_empty());
    }

    #[test]
    fn graph_rows_series_labels_mapped() {
        let mut m = std::collections::HashMap::new();
        m.insert("cpu_used_pct".to_string(), "CPU (%)".to_string());
        let rows = [
            json!({"ts": "t1", "series": "cpu_used_pct", "value": 12.0}),
            json!({"ts": "t1", "series": "mem_used_pct", "value": 45.0}),
        ];
        let s = normalize_graph_rows(&rows, "jarvis", Some(&m)).series;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].label, "CPU (%)"); // マップにあれば置換
        assert_eq!(s[1].label, "mem_used_pct"); // 無ければ素通し
    }

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

    fn th() -> crate::config::MeshThresholds {
        crate::config::MeshThresholds::default()
    }

    /// ext アドレスはすべてダミー（設置環境の実値は公開リポジトリに置かない）。
    fn sample() -> Value {
        json!({
          "timestamp": "2026-07-23T21:14:02+09:00",
          "network": {
            "name": "test-thread", "channel": 15,
            "partition_ids": [1581832881], "leader_router_id": 38
          },
          "nodes": [
            {"id": "ext:0011223344556677", "ext_address": "0011223344556677",
             "rloc16": "0x9800", "router_id": 38, "role": "leader"},
            {"id": "node:5", "node_id": 5, "alias": "desk_light",
             "role": "router", "probed": true},
            {"id": "ext:8899AABBCCDDEEFF", "ext_address": "8899AABBCCDDEEFF",
             "node_id": 7, "alias": "hall_light", "rloc16": "0x6c00",
             "router_id": 27, "role": "router", "probed": true},
            {"id": "ext:AABBCCDDEEFF0011", "ext_address": "AABBCCDDEEFF0011",
             "rloc16": "0x7c00", "router_id": 31, "role": "router",
             "label": "otbr-br"},
            {"id": "node:15", "node_id": 15, "role": "unknown", "probed": false,
             "probe_error": {"kind": "timeout", "detail": "no SRV+AAAA answer"}}
          ],
          "edges": [
            {"a": "node:5", "b": "ext:8899AABBCCDDEEFF",
             "a_sees_b": {"lqi": 3, "avg_rssi": -60, "last_rssi": -58,
                          "frame_error_rate": 0, "age": 12},
             "b_sees_a": {"lqi": 3, "avg_rssi": -62, "last_rssi": -61,
                          "frame_error_rate": 1, "age": 8}},
            {"a": "node:5", "b": "ext:AABBCCDDEEFF0011",
             "a_sees_b": null, "b_sees_a": null,
             "route": {"lqi_in": 3, "lqi_out": 3, "path_cost": 1}}
          ]
        })
    }

    #[test]
    fn mesh_network_and_counts() {
        let v = normalize_mesh(&sample(), &th(), None);
        assert_eq!(v.network.name.as_deref(), Some("test-thread"));
        assert_eq!(v.network.channel, Some(15));
        assert_eq!(v.network.leader_router_id, Some(38));
        assert!(!v.network.split);
        assert_eq!(v.fetched_at.as_deref(), Some("2026-07-23T21:14:02+09:00"));
        assert_eq!(v.nodes.len(), 5);
        assert_eq!(v.edges.len(), 2);
        // alias を持つのに ext を名乗れていないノード（node:5）だけを数える
        assert_eq!(v.unidentified_count, 1);
    }

    #[test]
    fn mesh_split_when_multiple_partitions() {
        let mut raw = sample();
        raw["network"]["partition_ids"] = json!([1, 2]);
        assert!(normalize_mesh(&raw, &th(), None).network.split);
    }

    #[test]
    fn mesh_node_kinds_and_names() {
        let v = normalize_mesh(&sample(), &th(), None);
        let by = |id: &str| v.nodes.iter().find(|n| n.id == id).unwrap();

        let leader = by("ext:0011223344556677");
        assert_eq!(leader.kind, "anonymous");
        assert_eq!(leader.name, "0011"); // ext の先頭 4 桁
        assert!(leader.is_leader);
        assert_eq!(leader.probed, None);

        let ours = by("node:5");
        assert_eq!(ours.kind, "ours");
        assert_eq!(ours.name, "desk_light"); // labels 未指定なので alias 素通し
        assert_eq!(ours.role, "router");
        assert_eq!(ours.probed, Some(true));
        assert!(!ours.is_leader);

        let named = by("ext:AABBCCDDEEFF0011");
        assert_eq!(named.kind, "named");
        assert_eq!(named.name, "otbr-br");

        let failed = by("node:15");
        assert_eq!(failed.kind, "anonymous"); // alias も label も無い
        assert_eq!(failed.probed, Some(false));
        assert_eq!(failed.error_kind.as_deref(), Some("timeout"));
    }

    #[test]
    fn mesh_labels_replace_alias() {
        let mut labels = std::collections::HashMap::new();
        labels.insert("desk_light".to_string(), "デスクライト".to_string());
        let v = normalize_mesh(&sample(), &th(), Some(&labels));
        let n = v.nodes.iter().find(|n| n.id == "node:5").unwrap();
        assert_eq!(n.name, "デスクライト");
    }

    #[test]
    fn mesh_route_only_edge() {
        let v = normalize_mesh(&sample(), &th(), None);
        let e = v.edges.iter().find(|e| e.b == "ext:AABBCCDDEEFF0011").unwrap();
        assert_eq!(e.grade, "route_only");
        assert_eq!(e.lqi, None);
        assert_eq!(e.rssi, None);
        assert_eq!(e.fer, None);
    }

    /// lqi だけで等級が決まるケース。
    #[test]
    fn mesh_grade_by_lqi() {
        for (lqi, want) in [(3u8, "good"), (2, "fair"), (1, "weak"), (0, "bad")] {
            let raw = json!({
              "network": {"partition_ids": []},
              "nodes": [{"id": "a"}, {"id": "b"}],
              "edges": [{"a": "a", "b": "b",
                         "a_sees_b": {"lqi": lqi, "avg_rssi": -70, "frame_error_rate": 0},
                         "b_sees_a": null}]
            });
            let v = normalize_mesh(&raw, &th(), None);
            assert_eq!(v.edges[0].grade, want, "lqi {lqi}");
        }
    }

    /// FER は等級を悪くする方向にしか働かない。good でも weak に落ち、bad は bad のまま。
    #[test]
    fn mesh_grade_fer_only_worsens() {
        let mk = |lqi: u8, fer: u8| {
            json!({
              "network": {"partition_ids": []},
              "nodes": [{"id": "a"}, {"id": "b"}],
              "edges": [{"a": "a", "b": "b",
                         "a_sees_b": {"lqi": lqi, "avg_rssi": -70, "frame_error_rate": fer},
                         "b_sees_a": null}]
            })
        };
        assert_eq!(normalize_mesh(&mk(3, 88), &th(), None).edges[0].grade, "weak");
        assert_eq!(normalize_mesh(&mk(2, 30), &th(), None).edges[0].grade, "weak");
        assert_eq!(normalize_mesh(&mk(0, 88), &th(), None).edges[0].grade, "bad");
        // しきい値未満の誤り率は等級を変えない
        assert_eq!(normalize_mesh(&mk(3, 29), &th(), None).edges[0].grade, "good");
    }

    /// 両視点のうち悪いほうを採る。LQI が同値なら誤り率が大きいほう。
    #[test]
    fn mesh_takes_worse_view() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 3, "avg_rssi": -55, "frame_error_rate": 0},
                     "b_sees_a": {"lqi": 1, "avg_rssi": -94, "frame_error_rate": 5}}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.grade, "weak");
        assert_eq!(e.lqi, Some(1));
        assert_eq!(e.rssi, Some(-94));
        assert_eq!(e.fer, Some(5));

        let tie = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 2, "avg_rssi": -70, "frame_error_rate": 4},
                     "b_sees_a": {"lqi": 2, "avg_rssi": -72, "frame_error_rate": 40}}]
        });
        let e = &normalize_mesh(&tie, &th(), None).edges[0];
        assert_eq!(e.fer, Some(40));
        assert_eq!(e.grade, "weak"); // 誤り率 40 >= 30 で fair から落ちる
    }

    #[test]
    fn mesh_thresholds_move_the_boundary() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 2, "avg_rssi": -70, "frame_error_rate": 0},
                     "b_sees_a": null}]
        });
        assert_eq!(normalize_mesh(&raw, &th(), None).edges[0].grade, "fair");
        let strict = crate::config::MeshThresholds { lqi_fair: 3, lqi_weak: 2, fer_weak: 30 };
        assert_eq!(normalize_mesh(&raw, &strict, None).edges[0].grade, "weak");
    }

    #[test]
    fn mesh_drops_edges_with_unknown_endpoint() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}],
          "edges": [{"a": "a", "b": "ghost", "a_sees_b": null, "b_sees_a": null}]
        });
        assert!(normalize_mesh(&raw, &th(), None).edges.is_empty());
    }

    /// 欠けている実測値をゼロ埋めしない（原則7）。lqi のみのビュー。
    #[test]
    fn mesh_link_lqi_only_no_zero_fill() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 1},
                     "b_sees_a": null}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.grade, "weak"); // lqi 1 は既定 lqi_weak=1 で weak
        assert_eq!(e.lqi, Some(1));
        assert_eq!(e.rssi, None);
        assert_eq!(e.fer, None);
        // 欠けた値は JSON に "0" ではなく、キーごと出ない。
        let s = serde_json::to_string(e).unwrap();
        assert!(!s.contains("\"rssi\""));
        assert!(!s.contains("\"fer\""));
    }

    /// fer だけ（lqi 欠落）がしきい値以上 → weak、lqi は出力に出ない。
    #[test]
    fn mesh_link_fer_only_above_threshold_is_weak() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"frame_error_rate": 90},
                     "b_sees_a": null}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.grade, "weak");
        assert_eq!(e.lqi, None);
        assert_eq!(e.fer, Some(90));
    }

    /// fer だけ（lqi 欠落）がしきい値未満 → route_only（弱いと偽らない）。
    #[test]
    fn mesh_link_fer_only_below_threshold_is_route_only() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"frame_error_rate": 5},
                     "b_sees_a": null}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.grade, "route_only");
        assert_eq!(e.lqi, None);
        assert_eq!(e.fer, Some(5));
    }

    /// 空のビューオブジェクト {} は品質測定なし＝route_only。0 を捏造しない。
    #[test]
    fn mesh_link_empty_object_is_route_only() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b", "a_sees_b": {}, "b_sees_a": null}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.grade, "route_only");
        assert_eq!(e.lqi, None);
        assert_eq!(e.rssi, None);
        assert_eq!(e.fer, None);
    }

    /// lqi が同値のとき、fer を持つほうが持たないほうより悪い（タイに負けない）。
    #[test]
    fn mesh_tie_break_prefers_view_with_fer() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 2},
                     "b_sees_a": {"lqi": 2, "frame_error_rate": 5}}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.lqi, Some(2));
        assert_eq!(e.fer, Some(5)); // fer を持つ視点が採られる
    }

    /// 上記の鏡像: fer を持つ視点を a_sees_b 側に置いても結果は変わらない。
    /// `is_worse_by_lqi` の (None, _) => false 分岐（= 既に選ばれている
    /// fer 持ちの視点を、後続の fer 欠落視点で上書きしない）を通す。
    #[test]
    fn mesh_tie_break_prefers_view_with_fer_mirrored_order() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 2, "frame_error_rate": 5},
                     "b_sees_a": {"lqi": 2}}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.lqi, Some(2));
        assert_eq!(e.fer, Some(5)); // 順序を入れ替えても fer を持つ視点が採られる
    }

    /// 片方は lqi のみ、もう片方は fer のみという部分的なビュー同士の組み合わせ。
    /// lqi を持つ視点がある時点でそちらが無条件に採られる（`pick_worst` は
    /// lqi 持ちの視点があれば fer のみの視点を見にいかない）ため、他方の
    /// fer はこの edge に一切混ざらない。これは「視点ごと」に選ぶ設計の
    /// 意図した帰結であり見落としではない — 将来この挙動を見直す余地は
    /// 残る（例えば異なる視点の fer を lqi 持ち視点に補完する等）。
    #[test]
    fn mesh_partial_views_lqi_view_wins_without_merging_other_views_fer() {
        let raw = json!({
          "network": {"partition_ids": []},
          "nodes": [{"id": "a"}, {"id": "b"}],
          "edges": [{"a": "a", "b": "b",
                     "a_sees_b": {"lqi": 3},
                     "b_sees_a": {"frame_error_rate": 90}}]
        });
        let e = &normalize_mesh(&raw, &th(), None).edges[0];
        assert_eq!(e.grade, "good");
        assert_eq!(e.lqi, Some(3));
        assert_eq!(e.fer, None); // b_sees_a の fer=90 は混ざらない
    }

    #[test]
    fn mesh_empty_graph() {
        let raw = json!({"network": {"partition_ids": []}, "nodes": [], "edges": []});
        let v = normalize_mesh(&raw, &th(), None);
        assert!(v.nodes.is_empty());
        assert!(v.edges.is_empty());
        assert!(!v.network.split);
        assert_eq!(v.network.name, None);
        assert_eq!(v.unidentified_count, 0);
        assert_eq!(v.fetched_at, None);
    }
}
