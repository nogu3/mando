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
}

async fn list_devices(State(app): State<Shared>) -> Json<Vec<DeviceInfo>> {
    let devices = app
        .config
        .devices
        .iter()
        .map(|d| DeviceInfo {
            name: d.name.clone(),
            label: d.label().to_string(),
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

async fn open_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.config.find(&name) {
        Some(device) => {
            let cmd = device.open.clone();
            Json(run_action(&app, device, &cmd).await).into_response()
        }
        None => not_found(&name),
    }
}

async fn close_device(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.config.find(&name) {
        Some(device) => {
            let cmd = device.close.clone();
            Json(run_action(&app, device, &cmd).await).into_response()
        }
        None => not_found(&name),
    }
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
