//! mando — スマートホーム操作の Web フロント。
//!
//! プロトコルは喋らない。casa（ブートストラップ期は enl）を subprocess で呼ぶ
//! だけの常駐 HTTP サービス。安定ミニ API を配り、フロントを下層から隔離する。

mod config;
mod exec;
mod normalize;

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::Value;

use config::{Config, Device, Kind};
use exec::{ExecOutcome, Executor};
use normalize::{normalize_enl_state, normalize_mat_onoff, State as DeviceState};

/// 焼き込んだ UI（設計原則 8）。config は外、UI はバイナリの一部。
const INDEX_HTML: &str = include_str!("../index.html");

struct App {
    config: Config,
    executor: Executor,
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
        .route("/api/groups", get(list_groups))
        .route("/api/groups/:name/open", post(group_open))
        .route("/api/groups/:name/close", post(group_close))
        .route("/api/groups/:name/stop", post(group_stop))
        .with_state(app)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Serialize)]
struct PresetInfo {
    name: String,
    label: String,
}

#[derive(Serialize)]
struct DeviceInfo {
    name: String,
    label: String,
    kind: Kind,
    /// stop 操作に対応しているか（UI が停止ボタンを出すか判断する）。
    stop: bool,
    /// light のプリセット（shutter は空）。
    presets: Vec<PresetInfo>,
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
            presets: d
                .presets
                .iter()
                .map(|p| PresetInfo {
                    name: p.name.clone(),
                    label: p.label().to_string(),
                })
                .collect(),
        })
        .collect();
    Json(devices)
}

/// state テンプレを exec → 正規化した結果。
#[derive(Serialize)]
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
    let result = app.executor.run(&device.get_state).await;

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

async fn get_state(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.config.find(&name) {
        Some(device) => Json(fetch_state(&app, device).await).into_response(),
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
    let result = app.executor.run(cmd).await;
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
    ActionView {
        action: result.outcome,
        state,
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

/// 名前付きプリセット exec → state 再取得（設計原則 7）。
async fn preset_device(
    State(app): State<Shared>,
    Path((name, preset)): Path<(String, String)>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    match device.preset_cmd(&preset) {
        Some(cmd) => Json(run_action(&app, device, cmd).await).into_response(),
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

async fn device_op(app: &App, name: &str, op: Op) -> Response {
    let Some(device) = app.config.find(name) else {
        return not_found(name);
    };
    match device_cmd(device, op) {
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
            r#"
            [[device]]
            name = "light"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            [[device.preset]]
            name  = "warm"
            label = "電球色"
            cmd   = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "shutter"
            get_state = ["sh", "-c", "printf '{\"properties\":[{\"name\":\"open_close_state\",\"value\":\"open\"}]}'"]
            open  = ["sh", "-c", "printf '{}'"]
            close = ["sh", "-c", "printf '{}'"]
            "#,
        )
        .unwrap();
        Arc::new(App {
            config: cfg,
            executor: Executor::new(),
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

    #[tokio::test]
    async fn devices_list_has_kind_and_presets() {
        let (st, v) = call("GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let light = arr.iter().find(|d| d["name"] == "light").unwrap();
        assert_eq!(light["kind"], "light");
        assert_eq!(light["presets"][0]["name"], "warm");
        assert_eq!(light["presets"][0]["label"], "電球色");
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
    async fn light_on_returns_confirmed_state() {
        let (st, v) = call("POST", "/api/devices/light/on").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        // 楽観表示ではなく再取得した確定値。
        assert_eq!(v["state"], "on");
    }

    #[tokio::test]
    async fn preset_runs_and_confirms_state() {
        let (st, v) = call("POST", "/api/devices/light/presets/warm").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["action"], "success");
        assert_eq!(v["state"], "on");
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
}
