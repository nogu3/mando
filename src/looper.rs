//! loop デバイス: config のコマンド配列を BPM の拍に合わせて exec し続ける。
//!
//! mando が知るのは「config の配列をタイミング付きで exec する」ことだけ。
//! 照明の群・色・パルスの形は config（設計原則 1・2）。
//!
//! - 拍は開始時刻からの絶対スケジュール（ドリフトしない）。処理が次の拍を越えたら
//!   越えた分は飛ばして未来の拍に揃える（遅れを積まない）。ただし遅れが 1/4 拍未満なら
//!   飛ばさない（小節頭の 3 exec が拍長を数 ms 越えても次のパルスを欠かさない）。
//! - 各コマンドは専用 Executor（graph / mesh と同じ流儀）で timeout 有界に exec。
//!   レーンは loop デバイス名（同じ loop の中は直列 — 色 → 100% → フェードの順が意味を持つ）。
//! - 拍の exec 失敗はループを止めない（演出で 1 拍落ちは許容）。連続 10 拍で warn 1 回。
//! - 状態は in-memory。mando 再起動で消える（起動時に on_stop は走らせない）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::exec::{ExecOutcome, Executor};

/// 連続失敗を warn にするしきい値（拍数）。
const FAIL_WARN_AFTER: u32 = 10;
/// shutdown 時に全 loop の on_stop に許す合計時間。
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
/// on_stop を再送するまでの間。Thread の multicast は burst の直後に捨てられる
/// （2026-10-07 実機: 停止直後の戻し 2 発が届かず、1.5 秒後の再送は届いた）ので、
/// 戻しは即時 1 回＋この間を空けてもう 1 回送る（冪等なので二重に届いても害はない）。
const ON_STOP_REPEAT_GAP: Duration = Duration::from_millis(1500);

/// config から組んだ 1 loop の中身（main が Device から作る）。
#[derive(Debug, Clone)]
pub struct LoopSpec {
    pub lane: String,
    pub bar_beats: u32,
    pub beat: Vec<Vec<String>>,
    pub bar: Vec<Vec<String>>,
    pub on_stop: Vec<Vec<String>>,
}

/// `GET state` / start / stop の応答。stopped では bpm / remaining_s を出さない。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoopStatus {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_s: Option<u64>,
}

impl LoopStatus {
    fn stopped() -> Self {
        LoopStatus {
            state: "stopped",
            bpm: None,
            remaining_s: None,
        }
    }
}

struct LoopRun {
    bpm: u32,
    deadline: Instant,
    /// true を送るとタスクが次の待ちで抜ける。
    cancel: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    /// 自動停止時にタスクが「自分のエントリ」だけを外すための世代番号。
    gen: u64,
}

pub struct Looper {
    executor: Executor,
    timeout: Duration,
    runs: Mutex<HashMap<String, LoopRun>>,
    next_gen: AtomicU64,
    /// on_stop の再送間隔。None なら 1 回だけ送る（テスト用）。
    on_stop_repeat: Option<Duration>,
}

/// `{fade}`: 拍長 ms × 0.8 を 0.1 秒単位で四捨五入（最小 1）。
pub fn fade_ds(bpm: u32) -> u64 {
    let beat_ms = 60_000 / bpm as u64;
    ((beat_ms * 8 + 500) / 1000).max(1)
}

impl Looper {
    pub fn new(timeout: Duration) -> Self {
        Self::with_on_stop_repeat(timeout, Some(ON_STOP_REPEAT_GAP))
    }

    /// on_stop の再送間隔を指定して作る（None = 再送しない）。
    pub fn with_on_stop_repeat(timeout: Duration, on_stop_repeat: Option<Duration>) -> Self {
        Looper {
            executor: Executor::new(),
            timeout,
            runs: Mutex::new(HashMap::new()),
            next_gen: AtomicU64::new(1),
            on_stop_repeat,
        }
    }

    pub fn status(&self, name: &str) -> LoopStatus {
        let runs = self.runs.lock().expect("runs poisoned");
        match runs.get(name) {
            Some(r) => LoopStatus {
                state: "running",
                bpm: Some(r.bpm),
                remaining_s: Some(
                    r.deadline
                        .saturating_duration_since(Instant::now())
                        .as_secs(),
                ),
            },
            None => LoopStatus::stopped(),
        }
    }

    /// 起動。running なら古いタスクを止めて置き換える（on_stop は走らせない —
    /// 直後に新ループが上書きする）。
    pub fn start(
        self: &Arc<Self>,
        name: &str,
        spec: LoopSpec,
        bpm: u32,
        max: Duration,
    ) -> LoopStatus {
        let (cancel, rx) = watch::channel(false);
        let now = Instant::now();
        let deadline = now + max;
        let gen = self.next_gen.fetch_add(1, Ordering::Relaxed);
        {
            // spawn と置き換えを 1 つのロックの下で行う。タスクが先に走っても
            // エントリが存在し（take_if_gen が成功する）、同名の同時 start でも
            // 古い方を必ず abort できる。spawn は await しないので std Mutex で足りる。
            let mut runs = self.runs.lock().expect("runs poisoned");
            let task = tokio::spawn(run_loop(
                self.clone(),
                name.to_string(),
                spec,
                bpm,
                deadline,
                rx,
                gen,
            ));
            let run = LoopRun {
                bpm,
                deadline,
                cancel,
                task,
                gen,
            };
            if let Some(old) = runs.insert(name.to_string(), run) {
                abort_run(old);
            }
        }
        tracing::info!(device = name, bpm, max_s = max.as_secs(), "loop 開始");
        self.status(name)
    }

    /// 停止して on_stop を順に exec。停止済みでも on_stop は exec する（照明を戻す用途で
    /// 冪等に押せる）。
    pub async fn stop(&self, name: &str, spec: &LoopSpec) -> LoopStatus {
        if let Some(run) = self.take(name) {
            abort_run(run);
            tracing::info!(device = name, "loop 停止");
        }
        self.run_on_stop(name, spec).await;
        LoopStatus::stopped()
    }

    /// shutdown: 全 loop を止め、running だったものの on_stop を合計 SHUTDOWN_BUDGET で exec。
    pub async fn shutdown(&self, specs: &[(String, LoopSpec)]) {
        let mut stopped = Vec::new();
        for (name, spec) in specs {
            if let Some(run) = self.take(name) {
                abort_run(run);
                stopped.push((name.as_str(), spec));
            }
        }
        if stopped.is_empty() {
            return;
        }
        // 再送の gap は全 loop で 1 回だけ空ける（loop ごとに待つと予算に収まらない）。
        let work = async {
            for (name, spec) in &stopped {
                self.run_on_stop_once(name, spec).await;
            }
            if let Some(gap) = self.on_stop_repeat {
                tokio::time::sleep(gap).await;
                for (name, spec) in &stopped {
                    self.run_on_stop_once(name, spec).await;
                }
            }
        };
        if tokio::time::timeout(SHUTDOWN_BUDGET, work).await.is_err() {
            tracing::warn!("shutdown: loop の on_stop が {SHUTDOWN_BUDGET:?} に収まらず打ち切り");
        }
    }

    fn take(&self, name: &str) -> Option<LoopRun> {
        self.runs.lock().expect("runs poisoned").remove(name)
    }

    /// 自動停止用: 自分の世代のエントリだけ外す（その間に start し直されていたら触らない）。
    fn take_if_gen(&self, name: &str, gen: u64) -> bool {
        let mut runs = self.runs.lock().expect("runs poisoned");
        if runs.get(name).is_some_and(|r| r.gen == gen) {
            runs.remove(name);
            true
        } else {
            false
        }
    }

    /// on_stop を送る: 即時 1 回＋（設定があれば）gap を空けてもう 1 回。
    async fn run_on_stop(&self, name: &str, spec: &LoopSpec) {
        self.run_on_stop_once(name, spec).await;
        if let Some(gap) = self.on_stop_repeat {
            tokio::time::sleep(gap).await;
            self.run_on_stop_once(name, spec).await;
        }
    }

    async fn run_on_stop_once(&self, name: &str, spec: &LoopSpec) {
        for cmd in &spec.on_stop {
            let r = self.exec(&spec.lane, cmd).await;
            if r.outcome != ExecOutcome::Success {
                tracing::warn!(
                    device = name,
                    outcome = ?r.outcome,
                    stderr = %r.stderr.trim(),
                    "on_stop 非成功"
                );
            }
        }
    }

    async fn exec(&self, lane: &str, cmd: &[String]) -> crate::exec::ExecResult {
        match tokio::time::timeout(self.timeout, self.executor.run(lane, cmd)).await {
            Ok(r) => r,
            Err(_) => crate::exec::ExecResult {
                outcome: ExecOutcome::Timeout,
                stdout: String::new(),
                stderr: "exec timeout".into(),
            },
        }
    }
}

/// cancel を送ってから abort する。exec 中の future は drop され、子プロセスは
/// kill_on_drop で回収される。
fn abort_run(run: LoopRun) {
    let _ = run.cancel.send(true);
    run.task.abort();
}

async fn run_loop(
    looper: Arc<Looper>,
    name: String,
    spec: LoopSpec,
    bpm: u32,
    deadline: Instant,
    mut cancel: watch::Receiver<bool>,
    gen: u64,
) {
    let beat_ms = 60_000 / bpm as u64;
    let beat_len = Duration::from_millis(beat_ms);
    let fade = fade_ds(bpm).to_string();
    let beat: Vec<Vec<String>> = spec
        .beat
        .iter()
        .map(|c| c.iter().map(|s| s.replace("{fade}", &fade)).collect())
        .collect();
    let bar_beats = spec.bar_beats.max(1) as u64;
    let start = Instant::now();
    let mut i: u64 = 0;
    let mut fails: u32 = 0;
    loop {
        let due = start + beat_len * (i as u32);
        if due >= deadline {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(due) => {}
            _ = cancel.changed() => return,
        }
        if *cancel.borrow() {
            return;
        }
        let mut ok = true;
        if !spec.bar.is_empty() && i.is_multiple_of(bar_beats) {
            let idx = ((i / bar_beats) as usize) % spec.bar.len();
            ok &= looper.exec(&spec.lane, &spec.bar[idx]).await.outcome == ExecOutcome::Success;
        }
        for cmd in &beat {
            ok &= looper.exec(&spec.lane, cmd).await.outcome == ExecOutcome::Success;
        }
        if ok {
            fails = 0;
        } else {
            fails += 1;
            if fails == FAIL_WARN_AFTER {
                tracing::warn!(device = %name, bpm, "拍の exec が {FAIL_WARN_AFTER} 拍連続で非成功");
            } else {
                tracing::debug!(device = %name, beat = i, "拍の exec 非成功");
            }
        }
        // 次の拍。処理が遅れて次の拍を越えていたら、越えた分は飛ばす（1/4 拍未満の遅れは許容）。
        // 1/4 拍未満の遅れは許容する（少し遅れたパルスの方が欠けたパルスより良い）。
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let slack = beat_ms / 4;
        i = (i + 1).max(elapsed_ms.saturating_sub(slack) / beat_ms + 1);
    }
    // 自動停止: 自分の世代のエントリだけ外し、on_stop を走らせる。
    if looper.take_if_gen(&name, gen) {
        tracing::info!(device = %name, "loop 自動停止（max_minutes）");
        looper.run_on_stop(&name, &spec).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("mando_loop_{tag}_{}.txt", std::process::id()));
        std::fs::write(&p, "").unwrap();
        p.to_string_lossy().to_string()
    }

    fn lines(p: &str) -> Vec<String> {
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect()
    }

    fn echo(p: &str, s: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), format!("echo {s} >> {p}")]
    }

    fn spec(p: &str) -> LoopSpec {
        LoopSpec {
            lane: "club".into(),
            bar_beats: 2,
            beat: vec![echo(p, "beat"), echo(p, "fade={fade}")],
            bar: vec![echo(p, "bar0"), echo(p, "bar1")],
            on_stop: vec![echo(p, "stop")],
        }
    }

    /// 既存テストは on_stop を 1 回だけ数えるので再送なしで組む（再送は専用テスト）。
    fn looper() -> Arc<Looper> {
        Arc::new(Looper::with_on_stop_repeat(Duration::from_secs(5), None))
    }

    #[test]
    fn fade_is_80_percent_of_beat_in_deciseconds_rounded() {
        assert_eq!(fade_ds(120), 4); // 500ms → 400ms → 4
        assert_eq!(fade_ds(128), 4); // 468ms → 375ms → 4（四捨五入）
        assert_eq!(fade_ds(100), 5); // 600ms → 480ms → 5
        assert_eq!(fade_ds(200), 2); // 300ms → 240ms → 2
    }

    #[tokio::test]
    async fn status_is_stopped_before_start() {
        let l = looper();
        let s = l.status("club");
        assert_eq!(s.state, "stopped");
        assert_eq!(s.bpm, None);
        assert_eq!(s.remaining_s, None);
    }

    #[tokio::test]
    async fn beats_and_bars_run_in_order_with_fade_substituted() {
        // 200 BPM = 300 ms/拍、bar_beats = 2。1.0 秒で拍 0,1,2,3 → bar0 beat fade bar? …
        let p = tmp("order");
        let l = looper();
        let s = l.start("club", spec(&p), 200, Duration::from_secs(60));
        assert_eq!(s.state, "running");
        assert_eq!(s.bpm, Some(200));
        assert!(s.remaining_s.unwrap() >= 59);
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let got = lines(&p);
        // 拍 0: bar0 beat fade=2 / 拍 1: beat fade=2 / 拍 2: bar1 beat fade=2 / 拍 3: beat fade=2
        let expect = [
            "bar0", "beat", "fade=2", "beat", "fade=2", "bar1", "beat", "fade=2", "beat", "fade=2",
        ];
        assert!(
            got.len() >= expect.len(),
            "1 秒で 4 拍分は走るはず: {got:?}"
        );
        assert_eq!(&got[..expect.len()], &expect);
        l.stop("club", &spec(&p)).await;
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn stop_runs_on_stop_and_clears_status() {
        let p = tmp("stop");
        let l = looper();
        l.start("club", spec(&p), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(350)).await;
        let s = l.stop("club", &spec(&p)).await;
        assert_eq!(s.state, "stopped");
        let got = lines(&p);
        assert_eq!(got.last().map(String::as_str), Some("stop"));
        assert_eq!(l.status("club").state, "stopped");
        // 止めた後は拍が増えない。
        let n = got.len();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(lines(&p).len(), n, "stop 後に拍が走った");
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn on_stop_is_repeated_after_gap() {
        // Thread の multicast は burst の直後に捨てられる（2026-10-07 実機: 停止直後の
        // 戻し 2 発が届かず、1.5 秒後の再送は届いた）。戻しは gap 空けて 2 回送る。
        let p = tmp("stop_repeat");
        let l = Arc::new(Looper::with_on_stop_repeat(
            Duration::from_secs(5),
            Some(Duration::from_millis(200)),
        ));
        let sp = LoopSpec {
            lane: "club".into(),
            bar_beats: 4,
            beat: vec![echo(&p, "beat")],
            bar: vec![],
            on_stop: vec![vec!["sh".into(), "-c".into(), format!("date +%s%N >> {p}")]],
        };
        let t0 = std::time::Instant::now();
        l.stop("club", &sp).await;
        assert!(
            t0.elapsed() >= Duration::from_millis(200),
            "stop は再送まで待って返る"
        );
        let ts: Vec<u128> = lines(&p).iter().filter_map(|l| l.parse().ok()).collect();
        assert_eq!(ts.len(), 2, "on_stop は 2 回送る: {ts:?}");
        assert!(
            (ts[1] - ts[0]) / 1_000_000 >= 200,
            "2 回目は gap 以上あとに送る"
        );
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn stop_when_stopped_still_runs_on_stop() {
        let p = tmp("stop_idem");
        let l = looper();
        let s = l.stop("club", &spec(&p)).await;
        assert_eq!(s.state, "stopped");
        assert_eq!(lines(&p), vec!["stop"]);
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn restart_replaces_running_loop_without_on_stop() {
        let p = tmp("restart");
        let l = looper();
        l.start("club", spec(&p), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(350)).await;
        let s = l.start("club", spec(&p), 100, Duration::from_secs(60));
        assert_eq!(s.bpm, Some(100));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let got = lines(&p);
        assert!(
            !got.iter().any(|l| l == "stop"),
            "合わせ直しでは on_stop を走らせない: {got:?}"
        );
        // 新ループは拍 0 から（bar0 で始まる）。古いループの出力の後に bar0 が再び現れる。
        assert!(got.iter().filter(|l| *l == "bar0").count() >= 2, "{got:?}");
        assert_eq!(l.status("club").bpm, Some(100));
        l.stop("club", &spec(&p)).await;
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn deadline_auto_stops_and_runs_on_stop() {
        let p = tmp("deadline");
        let l = looper();
        // 200 BPM、最長 700 ms → 拍 0, 300, 600 の 3 拍で止まる。
        l.start("club", spec(&p), 200, Duration::from_millis(700));
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let got = lines(&p);
        assert_eq!(got.last().map(String::as_str), Some("stop"), "{got:?}");
        assert_eq!(got.iter().filter(|l| *l == "beat").count(), 3, "{got:?}");
        assert_eq!(l.status("club").state, "stopped");
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn slow_beat_skips_to_next_future_beat() {
        // 拍のコマンドが 700 ms かかる（200 BPM = 300 ms/拍）。
        // 拍 0 が 700 ms で終わったら、拍 1・2 は飛ばして拍 3（900 ms）に揃う。
        let p = tmp("slow");
        let l = looper();
        let sp = LoopSpec {
            lane: "club".into(),
            bar_beats: 4,
            beat: vec![vec![
                "sh".into(),
                "-c".into(),
                format!("date +%s%N >> {p}; sleep 0.7"),
            ]],
            bar: vec![],
            on_stop: vec![echo(&p, "stop")],
        };
        l.start("club", sp.clone(), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(1300)).await;
        l.stop("club", &sp).await;
        let got = lines(&p);
        let ts: Vec<u128> = got.iter().filter_map(|l| l.parse().ok()).collect();
        assert!(ts.len() >= 2, "{got:?}");
        let gap_ms = (ts[1] - ts[0]) / 1_000_000;
        assert!(
            gap_ms >= 850,
            "遅れを積まず拍 3（900 ms）に揃うはず: gap={gap_ms}ms"
        );
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn slightly_late_beat_is_not_skipped() {
        // 200 BPM = 300 ms/拍、拍コマンドが 330 ms（30 ms 遅れ < 75 ms の許容）。
        // 次の拍は飛ばされず、連続拍（約 330 ms 間隔）になる。旧規則だと約 600 ms 間隔。
        // 遅れは積み上がるので、許容を使い切る前の約 1 秒（拍 0〜2）だけを見る。
        let p = tmp("late");
        let l = looper();
        let sp = LoopSpec {
            lane: "club".into(),
            bar_beats: 4,
            beat: vec![vec![
                "sh".into(),
                "-c".into(),
                format!("date +%s%N >> {p}; sleep 0.33"),
            ]],
            bar: vec![],
            on_stop: vec![],
        };
        l.start("club", sp.clone(), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(1000)).await;
        l.stop("club", &sp).await;
        let got = lines(&p);
        let ts: Vec<u128> = got.iter().filter_map(|l| l.parse().ok()).collect();
        assert!(ts.len() >= 3, "{got:?}");
        for w in ts.windows(2) {
            let gap_ms = (w[1] - w[0]) / 1_000_000;
            assert!(gap_ms < 450, "拍が飛ばされた: gap={gap_ms}ms {got:?}");
        }
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn failing_beats_do_not_stop_the_loop() {
        let p = tmp("fail");
        let l = looper();
        let sp = LoopSpec {
            lane: "club".into(),
            bar_beats: 4,
            beat: vec![vec![
                "sh".into(),
                "-c".into(),
                format!("echo x >> {p}; exit 1"),
            ]],
            bar: vec![],
            on_stop: vec![echo(&p, "stop")],
        };
        l.start("club", sp.clone(), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert_eq!(l.status("club").state, "running");
        assert!(lines(&p).len() >= 3, "失敗してもループは続く");
        l.stop("club", &sp).await;
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn shutdown_stops_all_and_runs_on_stop() {
        let p1 = tmp("sd1");
        let p2 = tmp("sd2");
        let l = looper();
        l.start("a", spec(&p1), 200, Duration::from_secs(60));
        l.start("b", spec(&p2), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(100)).await;
        l.shutdown(&[("a".into(), spec(&p1)), ("b".into(), spec(&p2))])
            .await;
        assert_eq!(lines(&p1).last().map(String::as_str), Some("stop"));
        assert_eq!(lines(&p2).last().map(String::as_str), Some("stop"));
        assert_eq!(l.status("a").state, "stopped");
        assert_eq!(l.status("b").state, "stopped");
        std::fs::remove_file(p1).ok();
        std::fs::remove_file(p2).ok();
    }

    #[tokio::test]
    async fn shutdown_bounds_on_stop_to_five_seconds() {
        let p = tmp("sd_slow");
        let l = looper();
        let sp = LoopSpec {
            lane: "club".into(),
            bar_beats: 4,
            beat: vec![echo(&p, "beat")],
            bar: vec![],
            on_stop: vec![vec!["sh".into(), "-c".into(), "sleep 30".into()]],
        };
        l.start("club", sp.clone(), 200, Duration::from_secs(60));
        let t0 = std::time::Instant::now();
        l.shutdown(&[("club".into(), sp)]).await;
        let took = t0.elapsed();
        assert!(
            took < Duration::from_secs(7),
            "shutdown は 5 秒で打ち切る: {took:?}"
        );
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn double_start_leaves_only_one_loop_running() {
        // 同名 start の連打: 古い方は必ず abort される（ハンドルを失って走り続けない）。
        let p = tmp("double");
        let l = looper();
        l.start("club", spec(&p), 200, Duration::from_secs(60));
        l.start("club", spec(&p), 200, Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(100)).await;
        l.stop("club", &spec(&p)).await;
        let n = lines(&p).len();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(lines(&p).len(), n, "stop 後も古いループが走っている");
        // 拍 0 の bar0 は 1 回だけ（2 本走っていれば 2 回出る）。
        assert_eq!(lines(&p).iter().filter(|l| *l == "bar0").count(), 1);
        std::fs::remove_file(p).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tiny_max_auto_stop_clears_entry_and_runs_on_stop() {
        // max が極小でも、エントリ挿入前にタスクが終わって running が残ることはない。
        for i in 0..20 {
            let p = tmp(&format!("tiny{i}"));
            let l = looper();
            l.start("club", spec(&p), 200, Duration::from_millis(0));
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(l.status("club").state, "stopped");
            assert_eq!(lines(&p), vec!["stop"]);
            std::fs::remove_file(p).ok();
        }
    }
}
