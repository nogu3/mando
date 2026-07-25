//! light 状態の push 取り込み（`mat listen` → in-memory）。
//!
//! 下層固有のイベント形の知識は `normalize.rs` に閉じ、このモジュールは
//! 「行を受け取り正規化関数に渡し、store を更新する」だけの下層非依存な
//! 機械に保つ（設計原則 4）。
//!
//! listener は `run_bounded` を通さない。あれは one-shot exec 用の
//! 「レーン直列化 + timeout」であり、無期限ストリームに適用すると即座に
//! 打ち切られる。listen は matd 経由で 3610 を掴まないためレーンも不要
//! （CLAUDE.md 原則 5 の「mat は matd が並行を捌くのでレーン不要」と同じ理由）。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::config::{Config, Kind};
use crate::normalize::{self, PushEvent, State};

/// 値の出どころ。UI が「いま何を根拠に表示しているか」を隠さないため（原則 7）。
pub const SOURCE_PUSH: &str = "push";
pub const SOURCE_READ: &str = "read";

/// listener 再起動の待ち（指数 backoff）。
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// これだけ生きていたなら一時的な事故とみなして backoff を初期値へ戻す
/// （戻さないと一度荒れたあと永久に上限で待つことになる）。
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);

/// デバイス 1 台の push 状態。
#[derive(Default)]
struct Slot {
    /// `"cluster/attribute"` → 最新値の汎用マップ。今 state に写すのは
    /// `onoff/on-off` だけ。明るさ・色は読み出す属性を足すだけで済む。
    attrs: HashMap<String, Value>,
}

impl Slot {
    /// 汎用マップから論理 state を導く。値が無い / 解釈できない値なら None
    /// （＝基準値未確立。呼び出し側は read で確定する）。
    fn state(&self) -> Option<State> {
        match self
            .attrs
            .get(normalize::ONOFF_KEY)
            .map(normalize::normalize_onoff_value)
        {
            Some(State::Unknown) | None => None,
            Some(s) => Some(s),
        }
    }
}

struct Inner {
    /// listener が生きているか。
    connected: bool,
    /// device 名 → slot。
    slots: HashMap<String, Slot>,
}

/// node_id → 論理デバイスの突合と、デバイスごとの最新属性値の in-memory 保持。
///
/// 鮮度は TTL で腐らせない。静止したライトの状態は勝手に変わらず、変われば
/// イベントが来る。信頼できるかどうかは listener が生きているかだけで決まる。
pub struct PushStore {
    /// 突合表。1 つの node_id が複数の論理デバイス（グループカードと
    /// そのメンバー等）の代表ノードになりうるので Vec で持つ。
    by_node: HashMap<u64, Vec<String>>,
    /// push 管理下のデバイス名（config 記載順）。
    tracked: Vec<String>,
    inner: Mutex<Inner>,
}

impl PushStore {
    /// config の `kind = "light"` かつ `node_id` ありのデバイスだけを対象に作る。
    pub fn new(config: &Config) -> Self {
        let mut by_node: HashMap<u64, Vec<String>> = HashMap::new();
        let mut tracked = Vec::new();
        for d in &config.devices {
            // node_id は light 以外の kind では無視する。
            if d.kind != Kind::Light {
                continue;
            }
            let Some(node_id) = d.node_id else { continue };
            by_node.entry(node_id).or_default().push(d.name.clone());
            tracked.push(d.name.clone());
        }
        PushStore {
            by_node,
            tracked,
            inner: Mutex::new(Inner {
                connected: false,
                slots: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("push store poisoned")
    }

    /// このデバイスが push 管理下か。
    pub fn tracks(&self, device: &str) -> bool {
        self.tracked.iter().any(|n| n == device)
    }

    /// primed（listener 接続中 かつ 基準値確立済み）なら push 値を返す。
    /// これが Some なら GET state は exec ゼロで即答できる。
    pub fn primed_state(&self, device: &str) -> Option<State> {
        let inner = self.lock();
        if !inner.connected {
            return None;
        }
        inner.slots.get(device)?.state()
    }

    /// listener の接続状態を切り替える。false にすると全デバイスの基準値を
    /// 捨てる（切れていた間に状態が変化した可能性があり、`mat listen` は
    /// 新規クライアント接続へ priming を replay しないため、再 read が唯一の
    /// 正しい復旧手段）。
    pub fn set_connected(&self, connected: bool) {
        let mut inner = self.lock();
        inner.connected = connected;
        if !connected {
            inner.slots.clear();
        }
    }

    /// read で確定した値を基準値として格納する（primed 化）。
    ///
    /// 基準値は listener が生きている間だけ意味を持つので、断中は何もしない
    /// （断中の read 結果は呼び出し元の GET state 応答として直接返る）。
    pub fn baseline(&self, device: &str, state: State) {
        let Some(value) = normalize::state_to_onoff_value(state) else {
            return;
        };
        let mut inner = self.lock();
        if !inner.connected {
            return;
        }
        Self::update(&mut inner, device, normalize::ONOFF_KEY.to_string(), value);
    }

    /// listener からの 1 イベントを取り込む。突合できる論理デバイスが
    /// 無ければ false（家には mando 管理外の Matter ノードが多数いる）。
    pub fn apply(&self, ev: &PushEvent) -> bool {
        let Some(devices) = self.by_node.get(&ev.node_id) else {
            return false;
        };
        let devices = devices.clone();
        let key = normalize::attr_key(&ev.cluster, &ev.attribute);
        let mut inner = self.lock();
        for device in &devices {
            Self::update(&mut inner, device, key.clone(), ev.value.clone());
        }
        true
    }

    /// 属性を 1 つ書く。
    fn update(inner: &mut Inner, device: &str, key: String, value: Value) {
        inner
            .slots
            .entry(device.to_string())
            .or_default()
            .attrs
            .insert(key, value);
    }
}

/// listen サブプロセスを回し続ける。落ちたら指数 backoff で再起動し、
/// そのたび全デバイスの基準値を捨てて再ベースライン read を依頼する
/// （read は購読の誘発も兼ね、これが cold-start を解消する）。
pub async fn run_listener(
    cmd: Vec<String>,
    store: Arc<PushStore>,
    rebaseline: mpsc::UnboundedSender<()>,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let started = Instant::now();
        match run_once(&cmd, &store, &rebaseline).await {
            Ok(status) => tracing::warn!(code = ?status.code(), "push listener が終了した"),
            Err(e) => tracing::warn!(error = %e, "push listener を起動できない"),
        }
        store.set_connected(false);
        if started.elapsed() >= BACKOFF_RESET_AFTER {
            backoff = BACKOFF_MIN;
        }
        tracing::info!(wait_ms = backoff.as_millis() as u64, "push listener を再起動する");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// listen を 1 回起動し、stdout が終わる（＝プロセスが落ちる）まで読み続ける。
async fn run_once(
    cmd: &[String],
    store: &Arc<PushStore>,
    rebaseline: &mpsc::UnboundedSender<()>,
) -> std::io::Result<std::process::ExitStatus> {
    let (program, args) = cmd.split_first().expect("validated non-empty command");
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // タスクが drop されたとき子プロセスを残さない。
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // stderr は読み捨てないとパイプが埋まって子が止まる。診断は debug に残す。
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(line = %line, "push listener stderr");
        }
    });

    // ack を待たず接続扱いにする（起動直後に落ちれば呼び出し側が戻す）。
    // 接続扱いを先にしないと、直後の再ベースライン read が基準値を
    // 格納できない（baseline は断中に何もしない）。
    store.set_connected(true);
    let _ = rebaseline.send(());

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        ingest(store, &line);
    }
    child.wait().await
}

/// 1 行を store へ反映する。壊れた行・管理外 node はその行だけ捨て、
/// ストリームは継続する（部分的な破損で全体を落とさない）。
pub(crate) fn ingest(store: &PushStore, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    match normalize::parse_mat_listen_event(line) {
        Some(ev) => {
            if !store.apply(&ev) {
                tracing::debug!(node_id = ev.node_id, "push: 管理外 node のイベント");
            }
        }
        None => tracing::debug!(line = %line, "push: 解釈できないイベント行"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// node 5 が living_lights と living_south_light の代表、node 6 が desk_light。
    /// plain は node_id 無しなので push 管理外（従来の read 経路）。
    const CFG: &str = r##"
        [push]
        listen = ["true"]
        [[device]]
        name = "living_lights"
        kind = "light"
        node_id = 5
        members = ["living_south_light"]
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
        [[device]]
        name = "living_south_light"
        kind = "light"
        node_id = 5
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
        [[device]]
        name = "desk_light"
        kind = "light"
        node_id = 6
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
        [[device]]
        name = "plain"
        kind = "light"
        get_state = ["true"]
        on = ["true"]
        off = ["true"]
    "##;

    fn store() -> PushStore {
        let cfg: Config = toml::from_str(CFG).unwrap();
        PushStore::new(&cfg)
    }

    /// 接続済み（listener 生存）の store。
    fn connected_store() -> PushStore {
        let s = store();
        s.set_connected(true);
        s
    }

    fn onoff_event(node_id: u64, on: bool) -> PushEvent {
        PushEvent {
            node_id,
            cluster: "onoff".into(),
            attribute: "on-off".into(),
            value: json!(on),
        }
    }

    #[test]
    fn tracks_only_lights_with_node_id() {
        let s = store();
        assert!(s.tracks("living_lights"));
        assert!(s.tracks("desk_light"));
        assert!(!s.tracks("plain"), "node_id 無しは push 管理外");
        assert!(!s.tracks("ghost"));
    }

    #[test]
    fn event_primes_all_devices_sharing_a_node() {
        let s = connected_store();
        assert!(s.apply(&onoff_event(5, true)));
        // 1 つの node_id が複数の論理デバイスの代表になりうる。
        assert_eq!(s.primed_state("living_lights"), Some(State::On));
        assert_eq!(s.primed_state("living_south_light"), Some(State::On));
        assert_eq!(s.primed_state("desk_light"), None);
    }

    #[test]
    fn unknown_node_is_ignored() {
        let s = connected_store();
        assert!(!s.apply(&onoff_event(99, true)), "管理外 node は false");
        assert_eq!(s.primed_state("living_lights"), None);
    }

    #[test]
    fn unprimed_while_disconnected_even_with_a_value() {
        let s = connected_store();
        s.apply(&onoff_event(6, true));
        assert_eq!(s.primed_state("desk_light"), Some(State::On));
        // 再接続時は全 light を unprimed に落として read で再ベースラインする。
        s.set_connected(false);
        assert_eq!(s.primed_state("desk_light"), None);
        s.set_connected(true);
        assert_eq!(
            s.primed_state("desk_light"),
            None,
            "再接続しただけで古い値を primed に戻してはいけない"
        );
    }

    #[test]
    fn baseline_primes_and_is_ignored_while_disconnected() {
        let s = store();
        s.baseline("desk_light", State::On);
        assert_eq!(s.primed_state("desk_light"), None, "断中は基準値を持たない");
        s.set_connected(true);
        s.baseline("desk_light", State::On);
        assert_eq!(s.primed_state("desk_light"), Some(State::On));
        // 解釈できない state は基準値にしない。
        s.baseline("living_lights", State::Unknown);
        assert_eq!(s.primed_state("living_lights"), None);
    }

    #[test]
    fn other_attributes_do_not_become_state() {
        let s = connected_store();
        // 明るさ・色の属性は汎用マップに入るが state には写らない。
        s.apply(&PushEvent {
            node_id: 6,
            cluster: "levelcontrol".into(),
            attribute: "current-level".into(),
            value: json!(120),
        });
        assert_eq!(s.primed_state("desk_light"), None);
        s.apply(&onoff_event(6, false));
        assert_eq!(s.primed_state("desk_light"), Some(State::Off));
    }

    #[tokio::test]
    async fn run_once_streams_lines_and_asks_for_rebaseline() {
        let s = Arc::new(store());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let script = concat!(
            r#"printf '{"node_id":6,"cluster":"onoff","attribute":"on-off","value":true}\n'; "#,
            r#"printf 'garbage\n'; "#,
            r#"printf '{"node_id":6,"cluster":"onoff","attribute":"on-off","value":false}\n'"#,
        );
        let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        let status = run_once(&cmd, &s, &tx).await.unwrap();
        assert!(status.success());
        assert!(rx.try_recv().is_ok(), "接続時に再ベースラインを依頼する");
        assert_eq!(
            s.primed_state("desk_light"),
            Some(State::Off),
            "壊れた行を挟んでもストリームは続く"
        );
    }

    #[tokio::test]
    async fn run_once_reports_spawn_failure() {
        let s = Arc::new(store());
        let (tx, _rx) = mpsc::unbounded_channel();
        let cmd = vec!["__mando_no_such_binary__".to_string()];
        assert!(run_once(&cmd, &s, &tx).await.is_err());
    }

    #[test]
    fn ingest_drops_bad_lines_and_keeps_going() {
        let s = connected_store();
        for line in [
            "",
            "   ",
            "not json",
            r#"{"listening":true}"#,
            r#"{"node_id":6,"cluster":"onoff","attribute":"on-off"}"#,
            r#"{"cluster":"onoff","attribute":"on-off","value":true}"#,
        ] {
            ingest(&s, line);
        }
        assert_eq!(s.primed_state("desk_light"), None, "壊れた行で state を作らない");
        // 壊れた行の後も正常な行は取り込める。
        ingest(
            &s,
            r#"{"timestamp":"t","node_id":6,"endpoint":1,"cluster":"onoff","attribute":"on-off","value":true,"priming":false,"recovered":false}"#,
        );
        assert_eq!(s.primed_state("desk_light"), Some(State::On));
    }

    #[test]
    fn ingest_takes_priming_and_recovered_events() {
        let s = connected_store();
        ingest(
            &s,
            r#"{"node_id":6,"cluster":"onoff","attribute":"on-off","value":true,"priming":true,"recovered":false}"#,
        );
        assert_eq!(
            s.primed_state("desk_light"),
            Some(State::On),
            "priming も recovered もその時点の実値を運ぶので受ける"
        );
    }
}
