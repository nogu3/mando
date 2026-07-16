//! subprocess の直列実行。
//!
//! enl は 0.0.0.0:3610 を専有 bind する。casa 経由でも casa が enl を呼ぶので
//! 透過的に同じ衝突が起きる。よって exec 全体を Semaphore(1) で囲い、
//! 並行に走らせない（axum は非同期だが、ここだけは意図的に直列）。

use serde::Serialize;
use std::process::Stdio;
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

/// exec 直列化器。全 subprocess 呼び出しを 1 本に絞る。
pub struct Executor {
    gate: Semaphore,
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            gate: Semaphore::new(1),
        }
    }

    /// コマンド配列を exec する。Semaphore(1) で直列化される。
    ///
    /// `cmd[0]` を実行ファイル、残りを引数として扱う。空配列は呼び出し側で
    /// 弾く前提（config validate 済み）。
    pub async fn run(&self, cmd: &[String]) -> ExecResult {
        let _permit = self.gate.acquire().await.expect("semaphore closed");

        let (program, args) = cmd.split_first().expect("empty command");
        tracing::debug!(program, ?args, "exec");

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
            .run(&["sh".into(), "-c".into(), "printf hello".into()])
            .await;
        assert_eq!(r.outcome, ExecOutcome::Success);
        assert_eq!(r.stdout, "hello");
    }

    #[tokio::test]
    async fn maps_nonzero_exit() {
        let ex = Executor::new();
        let r = ex.run(&["sh".into(), "-c".into(), "exit 3".into()]).await;
        assert_eq!(r.outcome, ExecOutcome::Timeout);
    }

    #[tokio::test]
    async fn reports_spawn_failure() {
        let ex = Executor::new();
        let r = ex.run(&["__mando_no_such_binary__".into()]).await;
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
                ex.run(&[
                    "sh".into(),
                    "-c".into(),
                    format!("printf 's{i} '>>{p}; sleep 0.05; printf 'e{i} '>>{p}"),
                ])
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
}
