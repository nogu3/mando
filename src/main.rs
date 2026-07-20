//! mando — スマートホーム操作の Web フロント。
//!
//! プロトコルは喋らない。casa（ブートストラップ期は enl）を subprocess で呼ぶ
//! だけの常駐 HTTP サービス。安定ミニ API を配り、フロントを下層から隔離する。

mod cache;
mod config;
mod exec;
mod normalize;

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use config::{Config, Device, Face, Kind};
use exec::{ExecOutcome, Executor};
use normalize::{normalize_enl_state, normalize_mat_onoff, GraphSeries, State as DeviceState};

/// 焼き込んだ UI（設計原則 8）。config は外、UI はバイナリの一部。
const INDEX_HTML: &str = include_str!("../index.html");

struct App {
    config: Config,
    executor: Executor,
    /// グラフ読み出し専用の直列化器。devices の executor（3610 衝突対策）とは
    /// 別枠 — 重い読み出し（duckdb 等）がシャッター操作をブロックしないため。
    /// グラフ同士は直列（ホストの CPU/メモリ保護）。
    graph_executor: Executor,
    /// state 読みの short-TTL + single-flight キャッシュ（原則6/7）。
    state_cache: cache::Cache<StateView>,
}

impl App {
    /// デバイス exec の上限（config の [exec] timeout_ms）。
    fn exec_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.exec.timeout_ms)
    }

    /// state 読みキャッシュの TTL（config の [cache] state_ttl_ms）。
    fn state_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.cache.state_ttl_ms)
    }
}

type Shared = Arc<App>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mando=info".into()),
        )
        .init();

    let config_path = std::env::var("MANDO_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[mando] {e} (path: {config_path})");
            eprintln!("[mando] MANDO_CONFIG で config.toml の場所を指定できる。サンプル: config.example.toml");
            std::process::exit(1);
        }
    };

    let bind = config.bind.clone();
    tracing::info!(
        devices = config.devices.len(),
        bind = %bind,
        "mando 起動"
    );

    let app = Arc::new(App {
        config,
        executor: Executor::new(),
        graph_executor: Executor::new(),
        state_cache: cache::Cache::default(),
    });

    let router = router(app);

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[mando] bind 失敗 {bind}: {e}");
            std::process::exit(1);
        }
    };

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown");
}

/// 安定ミニ API のルーティング（テストからも oneshot で叩く）。
fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/devices", get(list_devices))
        .route("/api/devices/:name/state", get(get_state))
        .route("/api/devices/:name/open", post(open_device))
        .route("/api/devices/:name/close", post(close_device))
        .route("/api/devices/:name/stop", post(stop_device))
        .route("/api/devices/:name/on", post(on_device))
        .route("/api/devices/:name/off", post(off_device))
        .route("/api/devices/:name/presets/:preset", post(preset_device))
        .route("/api/devices/:name/color", post(color_device))
        .route("/api/devices/:name/brightness", post(brightness_device))
        .route("/api/groups", get(list_groups))
        .route("/api/groups/:name/open", post(group_open))
        .route("/api/groups/:name/close", post(group_close))
        .route("/api/groups/:name/stop", post(group_stop))
        .route("/api/graphs", get(list_graphs))
        .route("/api/graphs/:name", get(get_graph))
        .route("/api/health", get(get_health))
        .with_state(app)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Serialize)]
struct PresetInfo {
    name: String,
    label: String,
    /// 色玉スウォッチ用 CSS color。None なら UI はテキストチップで出す。
    color: Option<String>,
}

#[derive(Serialize)]
struct DeviceInfo {
    name: String,
    label: String,
    kind: Kind,
    /// stop 操作に対応しているか（UI が停止ボタンを出すか判断する）。
    stop: bool,
    /// 任意色（color テンプレ）に対応しているか。UI がスライダーの出し分けに使う。
    color_supported: bool,
    /// 明るさ（brightness テンプレ）に対応しているか。UI がスライダーの出し分けに使う。
    brightness_supported: bool,
    /// light のプリセット（shutter は空）。
    presets: Vec<PresetInfo>,
    /// switch の表示フェイス（表示専用。null なら素のスイッチ）。
    face: Option<Face>,
    /// members を持つ light(グループカード)のメンバー device 名。空なら省略。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    members: Vec<String>,
}

async fn list_devices(State(app): State<Shared>) -> Json<Vec<DeviceInfo>> {
    let devices = app
        .config
        .devices
        .iter()
        .map(|d| DeviceInfo {
            name: d.name.clone(),
            label: d.label().to_string(),
            kind: d.kind,
            stop: d.stop_cmd().is_some(),
            color_supported: d.color_cmd().is_some(),
            brightness_supported: d.brightness_cmd().is_some(),
            presets: d
                .presets
                .iter()
                .map(|p| PresetInfo {
                    name: p.name.clone(),
                    label: p.label().to_string(),
                    color: p.color.clone(),
                })
                .collect(),
            face: d.face,
            members: d.members.clone(),
        })
        .collect();
    Json(devices)
}

/// state テンプレを exec → 正規化した結果。
#[derive(Serialize, Clone)]
struct StateView {
    /// 正規化された状態。shutter: open | closed | … / light: on | off。想定外は unknown。
    state: DeviceState,
    /// get_state の exec 結果（成否を正直に出す）。
    exec: ExecOutcome,
    /// 下層の生 JSON（パースできた場合のみ。デバッグ用）。
    raw: Option<Value>,
}

/// get_state を実行し、正規化した状態を返す。
async fn fetch_state(app: &App, device: &Device) -> StateView {
    let result = run_bounded(
        &app.executor,
        device.exec_lane(),
        &device.get_state,
        app.exec_timeout(),
    )
    .await;

    if result.outcome != ExecOutcome::Success {
        tracing::warn!(
            device = %device.name,
            outcome = ?result.outcome,
            stderr = %result.stderr.trim(),
            "get_state 非成功"
        );
        return StateView {
            state: DeviceState::Unknown,
            exec: result.outcome,
            raw: None,
        };
    }

    match serde_json::from_str::<Value>(&result.stdout) {
        Ok(raw) => StateView {
            state: match device.kind {
                Kind::Shutter => normalize_enl_state(&raw),
                Kind::Light => normalize_mat_onoff(&raw),
                Kind::Switch => normalize_enl_state(&raw),
            },
            exec: result.outcome,
            raw: Some(raw),
        },
        Err(e) => {
            tracing::warn!(device = %device.name, error = %e, "get_state JSON パース失敗");
            StateView {
                state: DeviceState::Unknown,
                exec: result.outcome,
                raw: None,
            }
        }
    }
}

/// get_state をキャッシュ経由で実行する（GET ハンドラ用）。
/// 成功読みだけ TTL キャッシュし、同時読みは 1 exec に合流する（原則6/7）。
/// set 経路はこれを通さず、生の fetch_state + store を使う。
async fn cached_state(app: &App, device: &Device) -> StateView {
    // light は原則7 の例外: catch-up 読みは代表ノードへの fresh なプロキシ読みで
    // あるべきで、共有 TTL キャッシュを経由すると古い状態を隠す窓を再導入してしまう。
    // また light は 3610 で他レーンと衝突せず、UI からポーリングもされないため
    // キャッシュの恩恵（直列待ち削減・重複 exec 抑制）もゼロ。よってキャッシュを
    // 経由せず常に fresh fetch_state を返す（store もしないので read/write とも
    // キャッシュに一切触れない）。
    if device.kind == Kind::Light {
        return fetch_state(app, device).await;
    }

    let ttl = app.state_ttl();
    app.state_cache
        .get_or_fetch(&device.name, ttl, || async {
            let view = fetch_state(app, device).await;
            let cacheable = view.exec == ExecOutcome::Success;
            (view, cacheable)
        })
        .await
}

async fn get_state(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.config.find(&name) {
        Some(device) => Json(cached_state(&app, device).await).into_response(),
        None => not_found(&name),
    }
}

/// set（open/close）の結果。set 後に必ず state を取り直す（楽観表示しない）。
#[derive(Serialize)]
struct ActionView {
    /// set コマンド自体の exec 結果。
    action: ExecOutcome,
    /// set 後に再取得した確定状態。
    #[serde(flatten)]
    state: StateView,
}

async fn run_action(app: &App, device: &Device, cmd: &[String]) -> ActionView {
    let result = run_bounded(&app.executor, device.exec_lane(), cmd, app.exec_timeout()).await;
    if result.outcome != ExecOutcome::Success {
        tracing::warn!(
            device = %device.name,
            outcome = ?result.outcome,
            stderr = %result.stderr.trim(),
            "set 非成功"
        );
    }
    // 設計原則 7: set 後は必ず state を取り直し、実際の開閉を確認してから返す。
    let state = fetch_state(app, device).await;
    // 確定値でキャッシュを更新（成功時のみ）。直後のポーリングが古い値を見ない。
    if state.exec == ExecOutcome::Success {
        app.state_cache
            .store(&device.name, state.clone())
            .await;
    }
    ActionView {
        action: result.outcome,
        state,
    }
}

/// light の set 結果。state は同梱しない — UI が押下 ~2 秒後に 1 回だけ
/// 追いつき取得する（設計原則 7 の light 例外。shutter は run_action を維持）。
#[derive(Serialize)]
struct LightActionView {
    action: ExecOutcome,
}

/// exec のみ実行して送信結果を返す（state 再取得なし）。light 用。
async fn run_light_action(app: &App, device: &Device, cmd: &[String]) -> LightActionView {
    let result = run_bounded(&app.executor, device.exec_lane(), cmd, app.exec_timeout()).await;
    if result.outcome != ExecOutcome::Success {
        tracing::warn!(
            device = %device.name,
            outcome = ?result.outcome,
            stderr = %result.stderr.trim(),
            "set 非成功"
        );
    }
    LightActionView {
        action: result.outcome,
    }
}

/// 操作の種類。
#[derive(Clone, Copy)]
enum Op {
    Open,
    Close,
    Stop,
    On,
    Off,
}

/// device の該当操作コマンドを返す。kind が対応しない操作は None。
fn device_cmd(device: &Device, op: Op) -> Option<Vec<String>> {
    match op {
        Op::Open => device.open_cmd().map(|c| c.to_vec()),
        Op::Close => device.close_cmd().map(|c| c.to_vec()),
        Op::Stop => device.stop_cmd().map(|c| c.to_vec()),
        Op::On => device.on_cmd().map(|c| c.to_vec()),
        Op::Off => device.off_cmd().map(|c| c.to_vec()),
    }
}

async fn open_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::Open).await
}

async fn close_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::Close).await
}

async fn stop_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::Stop).await
}

async fn on_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::On).await
}

async fn off_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    device_op(&app, &name, Op::Off).await
}

/// 名前付きプリセット exec → 送信結果のみ返す（light 例外: state は UI が追いつき取得）。
async fn preset_device(
    State(app): State<Shared>,
    Path((name, preset)): Path<(String, String)>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    match device.preset_cmd(&preset) {
        // preset は light 専用（config 検証済み）なので exec 結果のみ返す。
        Some(cmd) => Json(run_light_action(&app, device, cmd).await).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unknown preset\",\"name\":{},\"preset\":{}}}",
                json_str(&name),
                json_str(&preset)
            ),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ColorReq {
    color: String,
}

/// "#rrggbb"（大文字小文字可）のみ許す。検証済みの値だけが argv 置換に到達する。
fn valid_hex_color(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(|c| c.is_ascii_hexdigit())
}

/// 任意色 exec。テンプレの {color} を検証済み hex に置換して実行し、
/// 送信結果のみ返す（light 例外: state は UI が追いつき取得）。
async fn color_device(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Json(req): Json<ColorReq>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    // color テンプレの無い device（shutter 含む）は既存の kind 不整合と同じ 404。
    let Some(template) = device.color_cmd() else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unsupported operation\",\"name\":{}}}",
                json_str(&name)
            ),
        )
            .into_response();
    };
    if !valid_hex_color(&req.color) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"invalid color\",\"color\":{}}}",
                json_str(&req.color)
            ),
        )
            .into_response();
    }
    let cmd: Vec<String> = template
        .iter()
        .map(|s| s.replace("{color}", &req.color))
        .collect();
    Json(run_light_action(&app, device, &cmd).await).into_response()
}

#[derive(Deserialize)]
struct BrightnessReq {
    brightness: Value,
}

/// 明るさ exec。テンプレの {brightness} を検証済みの整数 1〜100 に置換して実行し、
/// 送信結果のみ返す（light 例外: state は UI が追いつき取得）。color_device の鏡像。
async fn brightness_device(
    State(app): State<Shared>,
    Path(name): Path<String>,
    Json(req): Json<BrightnessReq>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    // brightness テンプレの無い device（shutter 含む）は既存の kind 不整合と同じ 404。
    let Some(template) = device.brightness_cmd() else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unsupported operation\",\"name\":{}}}",
                json_str(&name)
            ),
        )
            .into_response();
    };
    // JSON 数値の整数のみ受ける。文字列・小数・0・101 以上・負値はすべて 400。
    let Some(level) = req.brightness.as_u64().filter(|n| (1..=100).contains(n)) else {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"invalid brightness\",\"brightness\":{}}}",
                req.brightness
            ),
        )
            .into_response();
    };
    let level = level.to_string();
    let cmd: Vec<String> = template
        .iter()
        .map(|s| s.replace("{brightness}", &level))
        .collect();
    Json(run_light_action(&app, device, &cmd).await).into_response()
}

async fn device_op(app: &App, name: &str, op: Op) -> Response {
    let Some(device) = app.config.find(name) else {
        return not_found(name);
    };
    match device_cmd(device, op) {
        // light は exec 結果のみ返す（state は UI が非同期に追いつき取得）。
        Some(cmd) if device.kind == Kind::Light => {
            Json(run_light_action(app, device, &cmd).await).into_response()
        }
        Some(cmd) => Json(run_action(app, device, &cmd).await).into_response(),
        // この kind では対応しない操作。
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unsupported operation\",\"name\":{}}}",
                json_str(name)
            ),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct GroupInfo {
    name: String,
    label: String,
    members: Vec<String>,
    /// 全メンバーが stop 対応していれば true（一括停止ボタンの出し分け）。
    stop: bool,
}

async fn list_groups(State(app): State<Shared>) -> Json<Vec<GroupInfo>> {
    let groups = app
        .config
        .groups
        .iter()
        .map(|g| {
            let stop = g
                .members
                .iter()
                .filter_map(|m| app.config.find(m))
                .all(|d| d.stop_cmd().is_some());
            GroupInfo {
                name: g.name.clone(),
                label: g.label().to_string(),
                members: g.members.clone(),
                stop,
            }
        })
        .collect();
    Json(groups)
}

/// グループ一括操作の 1 メンバー分の結果。
#[derive(Serialize)]
struct GroupMemberResult {
    name: String,
    /// stop 非対応で飛ばした場合 true。
    skipped: bool,
    #[serde(flatten)]
    result: ActionView,
}

async fn group_open(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    group_op(&app, &name, Op::Open).await
}

async fn group_close(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    group_op(&app, &name, Op::Close).await
}

async fn group_stop(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    group_op(&app, &name, Op::Stop).await
}

/// グループの全メンバーを記載順に操作する。各操作・state 再取得は Executor で
/// 直列化される（同時に 3610 を奪い合わない）。メンバーごとの結果を返す。
async fn group_op(app: &App, name: &str, op: Op) -> Response {
    let Some(group) = app.config.find_group(name) else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unknown group\",\"name\":{}}}",
                json_str(name)
            ),
        )
            .into_response();
    };

    let mut results = Vec::with_capacity(group.members.len());
    for member in &group.members {
        // validate 済みなので必ず見つかる。
        let device = app.config.find(member).expect("validated member");
        match device_cmd(device, op) {
            Some(cmd) => {
                let result = run_action(app, device, &cmd).await;
                results.push(GroupMemberResult {
                    name: member.clone(),
                    skipped: false,
                    result,
                });
            }
            // stop 非対応のメンバーは飛ばすが、現在 state は返す。
            None => {
                let state = fetch_state(app, device).await;
                results.push(GroupMemberResult {
                    name: member.clone(),
                    skipped: true,
                    result: ActionView {
                        action: ExecOutcome::Success,
                        state,
                    },
                });
            }
        }
    }
    Json(results).into_response()
}

fn not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        format!(
            "{{\"error\":\"unknown device\",\"name\":{}}}",
            json_str(name)
        ),
    )
        .into_response()
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

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
    let result = run_bounded(&app.graph_executor, "graph", &cmd, GRAPH_QUERY_TIMEOUT).await;
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
    let series =
        normalize::normalize_graph_rows(&rows, graph.label(), graph.series_labels.as_ref());
    Json(GraphView {
        name: graph.name.clone(),
        period: period.to_string(),
        unit: graph.unit_for(period).to_string(),
        series,
    })
    .into_response()
}

/// graph query の実行上限。下層 CLI の出力契約にタイムアウト保証がないため、
/// ハングした CLI が graph_executor（Semaphore(1)）を永久に握るのを防ぐ。
const GRAPH_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// exec を timeout 付きで走らせる。超過は Timeout として返す
/// （future の drop で permit は解放され、子プロセスは kill_on_drop で回収される）。
async fn run_bounded(
    executor: &Executor,
    lane: &str,
    cmd: &[String],
    timeout: std::time::Duration,
) -> exec::ExecResult {
    match tokio::time::timeout(timeout, executor.run(lane, cmd)).await {
        Ok(r) => r,
        Err(_) => exec::ExecResult {
            outcome: ExecOutcome::Timeout,
            stdout: String::new(),
            stderr: "exec timeout".into(),
        },
    }
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
    let result = run_bounded(
        &app.graph_executor,
        "graph",
        &health.command,
        GRAPH_QUERY_TIMEOUT,
    )
    .await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// sh で下層 CLI を偽装したテスト用 App。
    /// get_state は mat read / enl の実出力形式を printf で返す。
    fn test_app() -> Shared {
        let cfg: Config = toml::from_str(
            r##"
            [[device]]
            name = "light"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            color = ["sh", "-c", "test \"$1\" = '#ff69b4' && printf '{}'", "sh", "{color}"]
            brightness = ["sh", "-c", "test \"$1\" = '50' && printf '{}'", "sh", "{brightness}"]
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            color = "#ffd9a0"
            cmd   = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "plain"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "shutter"
            get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
            open  = ["sh", "-c", "printf '{}'"]
            close = ["sh", "-c", "printf '{}'"]
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
            [[graph]]
            name  = "machine"
            label = "jarvis"
            unit  = "%"
            query = ["sh", "-c", "printf '[{\"ts\":\"t1\",\"series\":\"cpu_used_pct\",\"value\":12.3},{\"ts\":\"t1\",\"series\":\"cpu_temp_c\",\"value\":52.0}]'", "sh", "{period}"]
            series_labels = { cpu_used_pct = "CPU (%)", cpu_temp_c = "温度 (℃)" }
            [health]
            label   = "jarvis"
            command = ["sh", "-c", "printf '[{\"metric\":\"cpu_used_pct\",\"value\":12.3,\"ts\":\"t1\",\"level\":\"ok\"},{\"metric\":\"disk_used_pct\",\"value\":83.2,\"ts\":\"t1\",\"level\":\"warn\"}]'"]
            [health.labels]
            cpu_used_pct  = "CPU"
            disk_used_pct = "ディスク"
            "##,
        )
        .unwrap();
        Arc::new(App {
            config: cfg,
            executor: Executor::new(),
            graph_executor: Executor::new(),
            state_cache: cache::Cache::default(),
        })
    }

    async fn call(method: &str, path: &str) -> (axum::http::StatusCode, Value) {
        let res = router(test_app())
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

    async fn call_json(method: &str, path: &str, body: &str) -> (axum::http::StatusCode, Value) {
        let res = router(test_app())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    async fn call_on(cfg_toml: &str, method: &str, path: &str) -> (axum::http::StatusCode, Value) {
        let app = Arc::new(App {
            config: toml::from_str(cfg_toml).unwrap(),
            executor: Executor::new(),
            graph_executor: Executor::new(),
            state_cache: cache::Cache::default(),
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

    #[tokio::test]
    async fn devices_list_has_kind_and_presets() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let light = arr.iter().find(|d| d["name"] == "light").unwrap();
        assert_eq!(light["kind"], "light");
        assert_eq!(light["presets"][0]["name"], "warm");
        assert_eq!(light["presets"][0]["label"], "電球色");
        assert_eq!(light["presets"][0]["color"], "#ffd9a0");
        let sh = arr.iter().find(|d| d["name"] == "shutter").unwrap();
        assert_eq!(sh["kind"], "shutter");
        assert_eq!(sh["presets"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn light_state_normalized_as_on() {
        let (st, v) = call("GET", "/api/devices/light/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "on");
    }

    #[tokio::test]
    async fn shutter_state_still_normalized_as_open() {
        let (st, v) = call("GET", "/api/devices/shutter/state").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "open");
    }

    #[tokio::test]
    async fn light_on_returns_action_only() {
        let (st, v) = call("POST", "/api/devices/light/on").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // 非同期確認: state は同梱しない（UI が後で 1 回だけ GET state する）。
        assert!(v.get("state").is_none());
        assert!(v.get("exec").is_none());
        assert!(v.get("raw").is_none());
    }

    #[tokio::test]
    async fn preset_returns_action_only() {
        let (st, v) = call("POST", "/api/devices/light/presets/warm").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        assert!(v.get("state").is_none());
    }

    #[tokio::test]
    async fn shutter_open_still_returns_confirmed_state() {
        let (st, v) = call("POST", "/api/devices/shutter/open").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // 設計原則 7: shutter は set 後の同期確認を維持。
        assert_eq!(v["state"], "open");
    }

    #[tokio::test]
    async fn kind_mismatch_is_404() {
        let (st, _) = call("POST", "/api/devices/light/open").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call("POST", "/api/devices/light/stop").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call("POST", "/api/devices/shutter/on").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call("POST", "/api/devices/shutter/presets/warm").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_preset_is_404() {
        let (st, v) = call("POST", "/api/devices/light/presets/nope").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(v["error"], "unknown preset");
    }

    #[tokio::test]
    async fn unknown_device_is_404() {
        let (st, _) = call("POST", "/api/devices/ghost/on").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn color_valid_hex_returns_action_only() {
        let (st, v) = call_json("POST", "/api/devices/light/color", r##"{"color":"#ff69b4"}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // light 例外: state は同梱しない。
        assert!(v.get("state").is_none());
    }

    #[tokio::test]
    async fn color_substitution_reaches_argv() {
        // 偽装 sh は "$1" = "#ff69b4" のときだけ成功する。別の正常 hex を送ると
        // 置換値がそのまま argv に渡っていれば failed になる。
        let (st, v) = call_json("POST", "/api/devices/light/color", r##"{"color":"#00ff00"}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "failed");
    }

    #[tokio::test]
    async fn color_invalid_hex_is_400() {
        for body in [
            r##"{"color":"#GGGGGG"}"##,
            r##"{"color":"red"}"##,
            r##"{"color":"#fff"}"##,
            r##"{"color":"#ff69b4aa"}"##,
        ] {
            let (st, _) = call_json("POST", "/api/devices/light/color", body).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    #[tokio::test]
    async fn color_without_template_is_404() {
        // テンプレ無し light / shutter / 未知 device はすべて既存の kind 不整合と同じ 404。
        for path in [
            "/api/devices/plain/color",
            "/api/devices/shutter/color",
            "/api/devices/ghost/color",
        ] {
            let (st, _) = call_json("POST", path, r##"{"color":"#ff69b4"}"##).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "path: {path}");
        }
    }

    #[tokio::test]
    async fn devices_list_has_color_supported() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let find = |n: &str| arr.iter().find(|d| d["name"] == n).unwrap();
        assert_eq!(find("light")["color_supported"], true);
        assert_eq!(find("plain")["color_supported"], false);
        assert_eq!(find("shutter")["color_supported"], false);
    }

    #[tokio::test]
    async fn brightness_valid_returns_action_only() {
        let (st, v) = call_json("POST", "/api/devices/light/brightness", r##"{"brightness":50}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // light 例外: state は同梱しない。
        assert!(v.get("state").is_none());
    }

    #[tokio::test]
    async fn brightness_substitution_reaches_argv() {
        // 偽装 sh は "$1" = "50" のときだけ成功する。別の正常値を送ると
        // 置換値がそのまま argv に渡っていれば failed になる。
        let (st, v) = call_json("POST", "/api/devices/light/brightness", r##"{"brightness":75}"##).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "failed");
    }

    #[tokio::test]
    async fn brightness_invalid_is_400() {
        for body in [
            r##"{"brightness":0}"##,
            r##"{"brightness":101}"##,
            r##"{"brightness":"50"}"##,
            r##"{"brightness":50.5}"##,
            r##"{"brightness":-1}"##,
        ] {
            let (st, _) = call_json("POST", "/api/devices/light/brightness", body).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    #[tokio::test]
    async fn brightness_without_template_is_404() {
        // テンプレ無し light / shutter / 未知 device はすべて既存の kind 不整合と同じ 404。
        for path in [
            "/api/devices/plain/brightness",
            "/api/devices/shutter/brightness",
            "/api/devices/ghost/brightness",
        ] {
            let (st, _) = call_json("POST", path, r##"{"brightness":50}"##).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "path: {path}");
        }
    }

    #[tokio::test]
    async fn devices_list_has_brightness_supported() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let find = |n: &str| arr.iter().find(|d| d["name"] == n).unwrap();
        assert_eq!(find("light")["brightness_supported"], true);
        assert_eq!(find("plain")["brightness_supported"], false);
        assert_eq!(find("shutter")["brightness_supported"], false);
    }

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

    #[tokio::test]
    async fn graph_query_timeout_maps_to_timeout_outcome() {
        let ex = Executor::new();
        let r = run_bounded(
            &ex,
            "graph",
            &["sh".into(), "-c".into(), "sleep 5".into()],
            std::time::Duration::from_millis(100),
        )
        .await;
        assert_eq!(r.outcome, ExecOutcome::Timeout);
        // permit が解放されていること（後続の run が即座に走れる）。
        let r2 = ex
            .run("graph", &["sh".into(), "-c".into(), "printf ok".into()])
            .await;
        assert_eq!(r2.outcome, ExecOutcome::Success);
        assert_eq!(r2.stdout, "ok");
    }

    #[tokio::test]
    async fn device_exec_times_out_and_maps_to_timeout_outcome() {
        use std::time::Instant;
        let config: Config = toml::from_str(
            r#"
            [exec]
            timeout_ms = 200

            [[device]]
            name = "slow"
            kind = "light"
            get_state = ["sleep", "60"]
            on = ["true"]
            off = ["true"]
            "#,
        )
        .unwrap();
        let app = App {
            config,
            executor: Executor::new(),
            graph_executor: Executor::new(),
            state_cache: cache::Cache::default(),
        };
        let device = app.config.find("slow").unwrap();
        let start = Instant::now();
        let view = fetch_state(&app, device).await;
        assert_eq!(view.exec, ExecOutcome::Timeout);
        assert_eq!(view.state, normalize::State::Unknown);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "timeout must bound the exec: {:?}",
            start.elapsed()
        );
    }

    /// get_state が exec のたびに temp ファイルへ 1 行追記する shutter を持つ App。
    /// `ttl_ms` でキャッシュ TTL を差し替える。
    fn counting_app(counter_path: &str, ttl_ms: u64) -> Shared {
        let cfg: Config = toml::from_str(&format!(
            r##"
            [cache]
            state_ttl_ms = {ttl_ms}
            [[device]]
            name = "shutter"
            get_state = ["sh", "-c", "printf x >> {counter_path}; printf '{{\"properties\":[{{\"name\":\"open_close_state\",\"value\":\"open\"}}]}}'"]
            open  = ["sh", "-c", "printf '{{}}'"]
            close = ["sh", "-c", "printf '{{}}'"]
            "##
        ))
        .unwrap();
        Arc::new(App {
            config: cfg,
            executor: Executor::new(),
            graph_executor: Executor::new(),
            state_cache: cache::Cache::default(),
        })
    }

    fn exec_count(counter_path: &str) -> usize {
        std::fs::read_to_string(counter_path)
            .map(|s| s.len())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn cached_state_hits_within_ttl() {
        let path = std::env::temp_dir().join(format!("mando_cache_hit_{}.txt", std::process::id()));
        let p = path.to_string_lossy().to_string();
        std::fs::write(&path, "").unwrap();

        let app = counting_app(&p, 2000);
        let device = app.config.find("shutter").unwrap();

        let a = cached_state(&app, device).await;
        let b = cached_state(&app, device).await;

        assert_eq!(a.state, normalize::State::Open);
        assert_eq!(b.state, normalize::State::Open);
        assert_eq!(exec_count(&p), 1, "TTL 内の 2 回目は exec しない");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn cached_state_refetches_after_ttl() {
        let path = std::env::temp_dir().join(format!("mando_cache_exp_{}.txt", std::process::id()));
        let p = path.to_string_lossy().to_string();
        std::fs::write(&path, "").unwrap();

        let app = counting_app(&p, 30);
        let device = app.config.find("shutter").unwrap();

        cached_state(&app, device).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        cached_state(&app, device).await;

        assert_eq!(exec_count(&p), 2, "TTL 経過後は再 exec");
        std::fs::remove_file(&path).ok();
    }

    /// get_state が exec のたびに temp ファイルへ 1 行追記する light を持つ App。
    /// counting_app の shutter 版に対応する light 版（原則7 light 例外の検証用）。
    fn counting_light_app(counter_path: &str, ttl_ms: u64) -> Shared {
        let cfg: Config = toml::from_str(&format!(
            r##"
            [cache]
            state_ttl_ms = {ttl_ms}
            [[device]]
            name = "light"
            kind = "light"
            get_state = ["sh", "-c", "printf x >> {counter_path}; printf '{{\"value\":true}}'"]
            on  = ["sh", "-c", "printf '{{}}'"]
            off = ["sh", "-c", "printf '{{}}'"]
            "##
        ))
        .unwrap();
        Arc::new(App {
            config: cfg,
            executor: Executor::new(),
            graph_executor: Executor::new(),
            state_cache: cache::Cache::default(),
        })
    }

    #[tokio::test]
    async fn light_state_bypasses_cache() {
        let path = std::env::temp_dir().join(format!("mando_cache_light_{}.txt", std::process::id()));
        let p = path.to_string_lossy().to_string();
        std::fs::write(&path, "").unwrap();

        let app = counting_light_app(&p, 2000);
        let device = app.config.find("light").unwrap();

        let a = cached_state(&app, device).await;
        let b = cached_state(&app, device).await;

        assert_eq!(a.state, normalize::State::On);
        assert_eq!(b.state, normalize::State::On);
        assert_eq!(
            exec_count(&p),
            2,
            "light は原則7 例外によりキャッシュされず毎回 exec する"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn devices_list_has_members_only_when_present() {
        let cfg = r##"
            [[device]]
            name = "parent"
            kind = "light"
            members = ["kid"]
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "kid"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
        "##;
        let (st, v) = call_on(cfg, "GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let parent = arr.iter().find(|d| d["name"] == "parent").unwrap();
        assert_eq!(parent["members"], serde_json::json!(["kid"]));
        // 空の members はフィールドごと省略される。
        let kid = arr.iter().find(|d| d["name"] == "kid").unwrap();
        assert!(kid.get("members").is_none());
    }
}
