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

use config::{Config, Device};
use exec::{ExecOutcome, Executor};
use normalize::{normalize_enl_state, State as DeviceState};

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

    let router = Router::new()
        .route("/", get(index))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/devices", get(list_devices))
        .route("/api/devices/:name/state", get(get_state))
        .route("/api/devices/:name/open", post(open_device))
        .route("/api/devices/:name/close", post(close_device))
        .route("/api/devices/:name/stop", post(stop_device))
        .route("/api/groups", get(list_groups))
        .route("/api/groups/:name/open", post(group_open))
        .route("/api/groups/:name/close", post(group_close))
        .route("/api/groups/:name/stop", post(group_stop))
        .with_state(app);

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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Serialize)]
struct DeviceInfo {
    name: String,
    label: String,
    /// stop 操作に対応しているか（UI が停止ボタンを出すか判断する）。
    stop: bool,
}

async fn list_devices(State(app): State<Shared>) -> Json<Vec<DeviceInfo>> {
    let devices = app
        .config
        .devices
        .iter()
        .map(|d| DeviceInfo {
            name: d.name.clone(),
            label: d.label().to_string(),
            stop: d.stop_cmd().is_some(),
        })
        .collect();
    Json(devices)
}

/// state テンプレを exec → 正規化した結果。
#[derive(Serialize)]
struct StateView {
    /// 正規化された開閉。open | closed | unknown。
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
            state: normalize_enl_state(&raw),
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
}

/// device の該当操作コマンドを返す。stop 非対応なら None。
fn device_cmd(device: &Device, op: Op) -> Option<Vec<String>> {
    match op {
        Op::Open => device.open_cmd().map(|c| c.to_vec()),
        Op::Close => device.close_cmd().map(|c| c.to_vec()),
        Op::Stop => device.stop_cmd().map(|c| c.to_vec()),
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

async fn device_op(app: &App, name: &str, op: Op) -> Response {
    let Some(device) = app.config.find(name) else {
        return not_found(name);
    };
    match device_cmd(device, op) {
        Some(cmd) => Json(run_action(app, device, &cmd).await).into_response(),
        // stop 非対応のデバイス。
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"stop unsupported\",\"name\":{}}}",
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
