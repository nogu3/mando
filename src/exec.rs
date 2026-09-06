//! subprocess のレーン単位直列実行。
//!
//! enl は 0.0.0.0:3610 を専有 bind する。casa 経由でも casa が enl を呼ぶので
//! 透過的に同じ衝突が起きる。echonet 系デバイスは config で lane = "echonet" に
//! まとめられ、同一レーンで直列化される。レーン未指定のデバイスはデバイス名が
//! レーンになり、自身の操作だけが直列（他デバイスとは並列）。mat は matd が
//! 並行を捌くのでレーン不要。timeout はこのモジュールでなく呼び出し側（run_bounded）が課す。
//! デバイス exec は config の [exec] timeout_ms（既定 15000）、graph/health は固定 30 秒。

use serde::Serialize;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tokio::sync::Semaphore;

/// enl 終了コード → 明確な UI 状態。
///
/// | code | 意味 | UI |
/// |---|---|---|
/// | 0 | success | 結果は state 再取得で確定 |
/// | 3 | timeout | 「応答なし、もう一度」 |
/// | 4 | device rejected (SNA) | 「機器が拒否」 |
/// | 5 | network/bind failure | 「ネットワーク異常」 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutcome {
    Success,
    Timeout,
    Rejected,
    NetworkError,
    /// 上記以外の非ゼロ終了。
    Failed,
    /// プロセス起動自体に失敗（コマンド不在など）。
    SpawnFailed,
}

impl ExecOutcome {
    fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => ExecOutcome::Success,
            Some(3) => ExecOutcome::Timeout,
            Some(4) => ExecOutcome::Rejected,
            Some(5) => ExecOutcome::NetworkError,
            _ => ExecOutcome::Failed,
        }
    }
}

#[derive(Debug)]
pub struct ExecResult {
    pub outcome: ExecOutcome,
    pub stdout: String,
    pub stderr: String,
}

/// exec 直列化器。レーン（文字列キー）ごとに Semaphore(1) を持ち、
/// 同一レーンの subprocess を直列化する。異なるレーンは並列に走る。
///
/// レーンの決め方は呼び出し側（config）の責務 — echonet 系（enl / casa 経由）は
/// 3610 を専有 bind するため同一レーンに集め、それ以外はデバイス単位でよい。
pub struct Executor {
    lanes: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            lanes: Mutex::new(HashMap::new()),
        }
    }

    /// レーンの Semaphore を取得（無ければ作る）。
    fn lane(&self, name: &str) -> Arc<Semaphore> {
        let mut lanes = self.lanes.lock().expect("lanes poisoned");
        lanes
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    /// コマンド配列を exec する。同一 lane 内で直列化される。
    ///
    /// `cmd[0]` を実行ファイル、残りを引数として扱う。空配列は呼び出し側で
    /// 弾く前提（config validate 済み）。
    pub async fn run(&self, lane: &str, cmd: &[String]) -> ExecResult {
        let sem = self.lane(lane);
        let _permit = sem.acquire_owned().await.expect("semaphore closed");

        let (program, args) = cmd.split_first().expect("empty command");
        tracing::debug!(lane, program, ?args, "exec");

        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // タイムアウト等で future が drop されたとき子プロセスを残さない。
            .kill_on_drop(true)
            .output()
            .await;

        match output {
            Ok(out) => {
                let outcome = ExecOutcome::from_code(out.status.code());
                ExecResult {
                    outcome,
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "spawn failed");
                ExecResult {
                    outcome: ExecOutcome::SpawnFailed,
                    stdout: String::new(),
                    stderr: e.to_string(),
                }
            }
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_exit_codes() {
        assert_eq!(ExecOutcome::from_code(Some(0)), ExecOutcome::Success);
        assert_eq!(ExecOutcome::from_code(Some(3)), ExecOutcome::Timeout);
        assert_eq!(ExecOutcome::from_code(Some(4)), ExecOutcome::Rejected);
        assert_eq!(ExecOutcome::from_code(Some(5)), ExecOutcome::NetworkError);
        assert_eq!(ExecOutcome::from_code(Some(1)), ExecOutcome::Failed);
        assert_eq!(ExecOutcome::from_code(None), ExecOutcome::Failed);
    }

    #[tokio::test]
    async fn runs_and_captures_stdout() {
        let ex = Executor::new();
        let r = ex
            .run("a", &["sh".into(), "-c".into(), "printf hello".into()])
            .await;
        assert_eq!(r.outcome, ExecOutcome::Success);
        assert_eq!(r.stdout, "hello");
    }

    #[tokio::test]
    async fn maps_nonzero_exit() {
        let ex = Executor::new();
        let r = ex
            .run("a", &["sh".into(), "-c".into(), "exit 3".into()])
            .await;
        assert_eq!(r.outcome, ExecOutcome::Timeout);
    }

    #[tokio::test]
    async fn reports_spawn_failure() {
        let ex = Executor::new();
        let r = ex.run("a", &["__mando_no_such_binary__".into()]).await;
        assert_eq!(r.outcome, ExecOutcome::SpawnFailed);
    }

    #[tokio::test]
    async fn serializes_concurrent_calls() {
        use std::sync::Arc;
        // 同時並行に走らせても直列化されることを、共有ファイルへの追記順で確認。
        let ex = Arc::new(Executor::new());
        let path = std::env::temp_dir().join(format!("mando_serial_{}.txt", std::process::id()));
        std::fs::write(&path, "").unwrap();
        let p = path.to_string_lossy().to_string();

        let mut handles = vec![];
        for i in 0..5 {
            let ex = ex.clone();
            let p = p.clone();
            handles.push(tokio::spawn(async move {
                // 各タスクは「開始マーカ → sleep → 終了マーカ」を書く。
                // 直列なら s,e ペアが交互に並ぶ。
                ex.run(
                    "a",
                    &[
                        "sh".into(),
                        "-c".into(),
                        format!("printf 's{i} '>>{p}; sleep 0.05; printf 'e{i} '>>{p}"),
                    ],
                )
                .await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        // 直列化されていれば、各 s の直後に対応する e が来る（割り込まれない）。
        let toks: Vec<&str> = content.split_whitespace().collect();
        for pair in toks.chunks(2) {
            assert_eq!(pair[0][..1].to_string(), "s");
            assert_eq!(pair[1][..1].to_string(), "e");
            assert_eq!(&pair[0][1..], &pair[1][1..], "interleaved exec: {content}");
        }
    }

    #[tokio::test]
    async fn different_lanes_run_in_parallel() {
        use std::sync::Arc;
        use std::time::Instant;
        // 0.3 秒 sleep を別レーンで同時に走らせ、直列（0.6 秒超）に
        // ならないことを経過時間で確認する。
        let ex = Arc::new(Executor::new());
        let start = Instant::now();
        let a = {
            let ex = ex.clone();
            tokio::spawn(async move { ex.run("lane_a", &["sleep".into(), "0.3".into()]).await })
        };
        let b = {
            let ex = ex.clone();
            tokio::spawn(async move { ex.run("lane_b", &["sleep".into(), "0.3".into()]).await })
        };
        a.await.unwrap();
        b.await.unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_millis(550),
            "different lanes should run in parallel: {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn same_lane_is_serialized_by_elapsed_time() {
        use std::sync::Arc;
        use std::time::Instant;
        let ex = Arc::new(Executor::new());
        let start = Instant::now();
        let a = {
            let ex = ex.clone();
            tokio::spawn(async move { ex.run("lane", &["sleep".into(), "0.3".into()]).await })
        };
        let b = {
            let ex = ex.clone();
            tokio::spawn(async move { ex.run("lane", &["sleep".into(), "0.3".into()]).await })
        };
        a.await.unwrap();
        b.await.unwrap();
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(600),
            "same lane must serialize: {:?}",
            start.elapsed()
        );
    }
}
