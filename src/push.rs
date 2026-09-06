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

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::config::{Config, Kind};
use crate::normalize::{self, PushEvent, State};

/// 値の出どころ。UI が「いま何を根拠に表示しているか」を隠さないため（原則 7）。
pub const SOURCE_PUSH: &str = "push";
pub const SOURCE_READ: &str = "read";

/// SSE で配る 1 件。接続直後のスナップショットも変化イベントも同じ形。
///
/// `stale` は構造上つねに false — イベントは信頼できる store からしか出ない。
/// フィールドを残しているのは、将来 listener 断そのものを通知する形に
/// 拡張したときに同じ形で運べるようにするため（今は live なデータではない）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateEvent {
    pub device: String,
    pub state: State,
    pub source: &'static str,
    pub stale: bool,
}

/// broadcast バッファ。溢れた遅いクライアントは Lagged になり、次の変化
/// イベント（or 再接続時のスナップショット）で追いつく。
const BROADCAST_CAPACITY: usize = 64;

/// listener 再起動の待ち（指数 backoff）。
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// これだけ生きていたなら一時的な事故とみなして backoff を初期値へ戻す
/// （戻さないと一度荒れたあと永久に上限で待つことになる）。
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);

/// stdout が終わったあと子の自然終了を待つ猶予。これを超えて残っていたら
/// kill する — ここで止まると `set_connected(false)` に到達せず、
/// 「生きているのに何も届かない」listener になる。
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(2);

/// prime（再ベースライン read）を頼むまでに子の生存を見る猶予。起動即死（mat が古い /
/// matd 不在）を backoff で繰り返す間、全 light の read を延々と撒かない。
const REBASELINE_DELAY: Duration = Duration::from_secs(3);

/// デバイス 1 台の push 状態。
#[derive(Default)]
struct Slot {
    /// `"cluster/attribute"` → 最新値の汎用マップ。今 state に写すのは
    /// `onoff/on-off` だけ。明るさ・色は読み出す属性を足すだけで済む。
    attrs: HashMap<String, Value>,
    /// `onoff/on-off` を最後に確定させた出どころ。state を導く値と同じ粒度で
    /// 持つ — slot 全体で 1 つにすると、明るさ・色のイベントが read 由来の
    /// onoff の出どころを "push" に書き換えてしまう。
    onoff_source: Option<&'static str>,
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

    /// state とその出どころ。どちらも `update` が同時に書くので、片方だけ
    /// 埋まっていることはない。
    fn state_with_source(&self) -> Option<(State, &'static str)> {
        Some((self.state()?, self.onoff_source?))
    }
}

struct Inner {
    /// listener が生きているか。
    connected: bool,
    /// 接続世代。`set_connected` のたびに進む。断を挟んだ read の戻りを
    /// 基準値として採用しないための世代印。
    generation: u64,
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
    /// push 管理下のデバイス名（config 記載順。スナップショットの順序）。
    tracked: Vec<String>,
    inner: Mutex<Inner>,
    /// close() で Sender を落とすと全購読者が Closed を受けて SSE が終端する。
    /// None = shutdown 済み（以後の subscribe は即 Closed、send は捨てる）。
    tx: Mutex<Option<broadcast::Sender<StateEvent>>>,
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
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        PushStore {
            by_node,
            tracked,
            inner: Mutex::new(Inner {
                connected: false,
                generation: 0,
                slots: HashMap::new(),
            }),
            tx: Mutex::new(Some(tx)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("push store poisoned")
    }

    /// このデバイスが push 管理下か。
    pub fn tracks(&self, device: &str) -> bool {
        self.tracked.iter().any(|n| n == device)
    }

    /// primed（listener 接続中 かつ 基準値確立済み）なら push 値とその出どころを
    /// 返す。これが Some なら GET state は exec ゼロで即答できる。
    pub fn primed_state(&self, device: &str) -> Option<(State, &'static str)> {
        let inner = self.lock();
        if !inner.connected {
            return None;
        }
        inner.slots.get(device)?.state_with_source()
    }

    /// 現在の接続世代。read を**始める前**に取り、`baseline` に渡す。
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// listener の接続状態を切り替える。切り替えのたびに全デバイスの基準値を
    /// 捨てる。断で捨てるのは必須（切れていた間に状態が変化した可能性があり、
    /// `mat listen` は新規クライアント接続へ priming を replay しないため、
    /// 再 read が唯一の正しい復旧手段）。復帰でも捨てるのは構造的な保証。
    ///
    /// 断そのものは broadcast しない — 静止したライトの値は断の間もほぼ
    /// 正しく、再接続直後の再ベースライン read が差分を必ず broadcast する。
    /// 断中に GET state を叩けば read フォールバック（失敗なら `stale: true`）
    /// で正直に出る。
    pub fn set_connected(&self, connected: bool) {
        let mut inner = self.lock();
        inner.connected = connected;
        // 断も復帰も世代を進める。跨いだ read の戻りは採用しない。
        inner.generation = inner.generation.wrapping_add(1);
        // 断で基準値を捨てるのは必須（切れていた間に変わった可能性がある）。
        // 復帰でも捨てるのは、断中に書かれた値が復帰で primed に昇格しない
        // ことを構造的に保証するため（実際には断中に書き手はいない）。
        inner.slots.clear();
    }

    /// read で確定した値を基準値として格納する（primed 化）。
    ///
    /// `generation` は read を**始める前**に `generation()` で取った値。
    /// 断・再接続を跨いだ read の戻りは基準値にしない — 切れていた間に状態が
    /// 変わった可能性があり、`mat listen` は新規接続へ priming を replay
    /// しないので、その値が今も正しい保証がない。
    ///
    /// 基準値は listener が生きている間だけ意味を持つので、断中も何もしない
    /// （断中の read 結果は呼び出し元の GET state 応答として直接返る）。
    pub fn baseline(&self, device: &str, state: State, generation: u64) {
        // 管理外のデバイスの slot は作らない（store が持つのは push 管理下だけ）。
        if !self.tracks(device) {
            return;
        }
        let Some(value) = normalize::state_to_onoff_value(state) else {
            return;
        };
        let mut inner = self.lock();
        if !inner.connected || inner.generation != generation {
            return;
        }
        self.update(
            &mut inner,
            device,
            normalize::ONOFF_KEY.to_string(),
            value,
            SOURCE_READ,
        );
    }

    /// 操作を送ったので基準値を落とす。次の GET は read で実機を見に行く
    /// （push イベントが来ればそれが基準値になり、read は起きない）。
    /// 送信できたことは状態の確認ではないので、押下前の値を primed の
    /// まま出し続けてはいけない（原則 7）。
    ///
    /// 同じ代表ノードを共有する論理デバイス（グループカードとそのメンバー）は
    /// まとめて落とす — `apply` がイベントを全員に配るのと同じ範囲。1 台への
    /// 操作が他方の実状態も動かすので、片方だけ primed に残してはいけない。
    pub fn invalidate(&self, device: &str) {
        let siblings: Vec<String> = self
            .by_node
            .values()
            .find(|devices| devices.iter().any(|d| d == device))
            .cloned()
            .unwrap_or_else(|| vec![device.to_string()]);
        let mut inner = self.lock();
        for d in &siblings {
            inner.slots.remove(d);
        }
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
            self.update(
                &mut inner,
                device,
                key.clone(),
                ev.value.clone(),
                SOURCE_PUSH,
            );
        }
        true
    }

    /// 属性を 1 つ書き、導出 state が変わったときだけ broadcast する
    /// （onoff を動かさない属性の更新でクライアントを起こさない）。
    fn update(
        &self,
        inner: &mut Inner,
        device: &str,
        key: String,
        value: Value,
        source: &'static str,
    ) {
        let slot = inner.slots.entry(device.to_string()).or_default();
        let before = slot.state();
        let is_onoff = key == normalize::ONOFF_KEY;
        slot.attrs.insert(key, value);
        if is_onoff {
            slot.onoff_source = Some(source);
        }
        let after = slot.state();
        if after == before {
            return;
        }
        if let Some(state) = after {
            // 購読者ゼロなら Err。捨ててよい。close 後（None）も同様に捨てる。
            if let Some(tx) = self.tx.lock().expect("push tx poisoned").as_ref() {
                let _ = tx.send(StateEvent {
                    device: device.to_string(),
                    state,
                    source,
                    stale: false,
                });
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateEvent> {
        match self.tx.lock().expect("push tx poisoned").as_ref() {
            Some(tx) => tx.subscribe(),
            // close 済み: Sender を即座に落とした受け口を返す。購読者は
            // 最初の recv で Closed を受け、SSE は開始と同時に終端する。
            None => broadcast::channel(1).1,
        }
    }

    /// shutdown 用。broadcast の Sender を落とし、全 SSE ストリームを終端させる。
    /// これが無いと graceful shutdown が無限ストリームの完了を待ち続け、
    /// systemd の TimeoutStopSec で SIGKILL に落ちる。
    pub fn close(&self) {
        self.tx.lock().expect("push tx poisoned").take();
    }

    /// SSE 接続直後に送る現在スナップショット。基準値が確立していない
    /// デバイスは含めない — 「不明」で上書きして、クライアントが read で
    /// 得た正しい表示を壊さないため。
    pub fn snapshot(&self) -> Vec<StateEvent> {
        let inner = self.lock();
        if !inner.connected {
            return Vec::new();
        }
        self.tracked
            .iter()
            .filter_map(|name| {
                let (state, source) = inner.slots.get(name)?.state_with_source()?;
                Some(StateEvent {
                    device: name.clone(),
                    state,
                    source,
                    stale: false,
                })
            })
            .collect()
    }
}

/// listen サブプロセスを回し続ける。落ちたら指数 backoff で再起動し、
/// そのたび全デバイスの基準値を捨てて prime（基準値 read）を依頼する
/// （read は購読の誘発も兼ね、これが cold-start を解消する）。
pub async fn run_listener(
    cmd: Vec<String>,
    store: Arc<PushStore>,
    rebaseline: mpsc::UnboundedSender<()>,
) {
    let mut backoff = BACKOFF_MIN;
    // 連続した断の回数。健全に生きたあとの断でリセットする（backoff と同じ判定）。
    let mut losses: u32 = 0;
    loop {
        let started = Instant::now();
        let outcome = run_once(&cmd, &store, &rebaseline).await;
        store.set_connected(false);
        if started.elapsed() >= BACKOFF_RESET_AFTER {
            backoff = BACKOFF_MIN;
            losses = 0;
        }
        losses += 1;
        // 単発〜2 回の断は matd の restart / コンテナ再作成で日常的に起きる想定内
        // （数秒で戻り、prime ループが基準値を取り直す）。続くなら warn。
        // code=None は猶予超過で mando が kill した場合（直前に warn 済み）。
        match (&outcome, listener_loss_is_alarming(losses)) {
            (Ok(status), true) => {
                tracing::warn!(code = ?status.code(), losses, "push listener が終了した（断が続く）")
            }
            (Ok(status), false) => {
                tracing::info!(code = ?status.code(), losses, "push listener が終了した")
            }
            (Err(e), true) => {
                tracing::warn!(error = %e, losses, "push listener を起動できない（断が続く）")
            }
            (Err(e), false) => tracing::info!(error = %e, losses, "push listener を起動できない"),
        }
        tracing::info!(
            wait_ms = backoff.as_millis() as u64,
            "push listener を再起動する"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// 連続 `losses` 回目の listener 断を warn にするか。1〜2 回は想定内（info）。
fn listener_loss_is_alarming(losses: u32) -> bool {
    losses >= 3
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

    // stderr は読み捨てないとパイプが埋まって子が止まる。止まると stdout が
    // EOF にならず、listener が「生きているのに何も届かない」状態に陥って
    // primed 値を信頼できるものとして出し続けてしまう。不正な UTF-8 で
    // 降りないよう、行ではなくバイトで読んで lossy に落とす。
    tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buf = [0u8; 1024];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    tracing::debug!(chunk = %chunk.trim_end(), "push listener stderr");
                }
                Err(e) => {
                    tracing::debug!(error = %e, "push listener stderr の読み取りを終了");
                    break;
                }
            }
        }
    });

    // ack を待たず接続扱いにする（起動直後に落ちれば呼び出し側が戻す）。
    // 接続扱いを先にしないと、直後の再ベースライン read が基準値を
    // 格納できない（baseline は断中に何もしない）。
    store.set_connected(true);
    // 再ベースラインは子が少し生き延びてから頼む。即死した場合は
    // set_connected(false) が世代を進めるので、この依頼は捨てられる。
    {
        let store = store.clone();
        let rebaseline = rebaseline.clone();
        let generation = store.generation();
        tokio::spawn(async move {
            tokio::time::sleep(REBASELINE_DELAY).await;
            if store.generation() == generation {
                let _ = rebaseline.send(());
            }
        });
    }

    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => ingest(store, &line),
            Ok(None) => break,
            Err(e) => {
                // ストリーム読み取りの失敗は「終わった」として扱い、終了
                // ステータスは下で観測する（起動失敗と混同しない）。
                tracing::warn!(error = %e, "push listener stdout の読み取りに失敗");
                break;
            }
        }
    }
    // stdout が終わった = プロセスが落ちた、もしくは読めなくなった。
    // 子が生きたまま抜けた場合に wait で永久に止まらないよう、待ちを
    // 有界にしてから kill する。
    match tokio::time::timeout(CHILD_EXIT_GRACE, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            tracing::warn!("push listener が stdout 終了後も残っている: kill する");
            let _ = child.start_kill();
            child.wait().await
        }
    }
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
        [[device]]
        name = "blind"
        node_id = 7
        get_state = ["true"]
        open = ["true"]
        close = ["true"]
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

    /// 出どころを無視して state だけ見る（多くのテストは出どころに関心がない）。
    fn primed(s: &PushStore, device: &str) -> Option<State> {
        s.primed_state(device).map(|(st, _)| st)
    }

    #[test]
    fn tracks_only_lights_with_node_id() {
        let s = store();
        assert!(s.tracks("living_lights"));
        assert!(s.tracks("desk_light"));
        assert!(!s.tracks("plain"), "node_id 無しは push 管理外");
        assert!(!s.tracks("ghost"));
        assert!(
            !s.tracks("blind"),
            "node_id があっても light 以外は push 管理外"
        );
    }

    #[test]
    fn event_primes_all_devices_sharing_a_node() {
        let s = connected_store();
        assert!(s.apply(&onoff_event(5, true)));
        // 1 つの node_id が複数の論理デバイスの代表になりうる。
        assert_eq!(primed(&s, "living_lights"), Some(State::On));
        assert_eq!(primed(&s, "living_south_light"), Some(State::On));
        assert_eq!(primed(&s, "desk_light"), None);
    }

    #[test]
    fn unknown_node_is_ignored() {
        let s = connected_store();
        assert!(!s.apply(&onoff_event(99, true)), "管理外 node は false");
        assert_eq!(primed(&s, "living_lights"), None);
    }

    #[test]
    fn unprimed_while_disconnected_even_with_a_value() {
        let s = connected_store();
        s.apply(&onoff_event(6, true));
        assert_eq!(primed(&s, "desk_light"), Some(State::On));
        // 再接続時は全 light を unprimed に落として read で再ベースラインする。
        s.set_connected(false);
        assert_eq!(primed(&s, "desk_light"), None);
        s.set_connected(true);
        assert_eq!(
            primed(&s, "desk_light"),
            None,
            "再接続しただけで古い値を primed に戻してはいけない"
        );
    }

    #[test]
    fn baseline_primes_and_is_ignored_while_disconnected() {
        let s = store();
        s.baseline("desk_light", State::On, s.generation());
        assert_eq!(primed(&s, "desk_light"), None, "断中は基準値を持たない");
        s.set_connected(true);
        s.baseline("desk_light", State::On, s.generation());
        assert_eq!(primed(&s, "desk_light"), Some(State::On));
        // 解釈できない state は基準値にしない。
        s.baseline("living_lights", State::Unknown, s.generation());
        assert_eq!(primed(&s, "living_lights"), None);
    }

    #[test]
    fn baseline_from_a_read_that_straddled_a_disconnect_is_dropped() {
        let s = connected_store();
        // read 開始時の世代を取ったあとで listener が落ちて復帰する。
        let generation = s.generation();
        s.set_connected(false);
        s.set_connected(true);
        s.baseline("desk_light", State::On, generation);
        assert_eq!(
            primed(&s, "desk_light"),
            None,
            "断を跨いだ read の戻りを基準値にしてはいけない"
        );
        // 現在の世代の read はちゃんと採用される。
        s.baseline("desk_light", State::On, s.generation());
        assert_eq!(primed(&s, "desk_light"), Some(State::On));
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
        assert_eq!(primed(&s, "desk_light"), None);
        s.apply(&onoff_event(6, false));
        assert_eq!(primed(&s, "desk_light"), Some(State::Off));
    }

    #[test]
    fn onoff_source_is_not_overwritten_by_other_attributes() {
        let s = connected_store();
        s.baseline("desk_light", State::On, s.generation());
        assert_eq!(
            s.primed_state("desk_light"),
            Some((State::On, SOURCE_READ)),
            "read で確立した基準値の出どころは read"
        );
        // 明るさイベントは onoff の出どころを書き換えない。
        s.apply(&PushEvent {
            node_id: 6,
            cluster: "levelcontrol".into(),
            attribute: "current-level".into(),
            value: json!(120),
        });
        assert_eq!(
            s.primed_state("desk_light"),
            Some((State::On, SOURCE_READ)),
            "onoff を動かさない属性が出どころを書き換えてはいけない"
        );
        // onoff の push が来たら出どころは push になる。
        s.apply(&onoff_event(6, false));
        assert_eq!(
            s.primed_state("desk_light"),
            Some((State::Off, SOURCE_PUSH))
        );
    }

    #[test]
    fn invalidate_drops_the_baseline() {
        let s = connected_store();
        s.apply(&onoff_event(6, true));
        assert_eq!(primed(&s, "desk_light"), Some(State::On));
        // 操作を送ったら基準値を落とす（送信は確認ではない）。
        s.invalidate("desk_light");
        assert_eq!(primed(&s, "desk_light"), None);
    }

    #[test]
    fn invalidate_covers_devices_sharing_a_node() {
        let s = connected_store();
        s.apply(&onoff_event(5, true));
        assert_eq!(primed(&s, "living_lights"), Some(State::On));
        assert_eq!(primed(&s, "living_south_light"), Some(State::On));
        // グループカードへの操作は同じ代表ノードのメンバーも動かす。
        s.invalidate("living_lights");
        assert_eq!(primed(&s, "living_lights"), None);
        assert_eq!(
            primed(&s, "living_south_light"),
            None,
            "同じ代表ノードを共有するデバイスも基準値を落とす"
        );
    }

    /// shutdown で SSE を終端するための釘: close() で既存購読者は Closed を
    /// 受け、以後の subscribe も即 Closed になる（graceful shutdown が
    /// 無限ストリームを待ち続けない）。
    #[test]
    fn close_ends_existing_and_future_subscribers() {
        let s = connected_store();
        let mut rx = s.subscribe();
        s.close();
        assert!(
            matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)),
            "close 後、既存購読者は Closed を受ける"
        );
        let mut fresh = s.subscribe();
        assert!(
            matches!(
                fresh.try_recv(),
                Err(broadcast::error::TryRecvError::Closed)
            ),
            "close 後の subscribe は即 Closed（shutdown 中の新規 SSE を待たせない）"
        );
    }

    /// close 後に listen イベントや baseline が届いても落ちない
    /// （shutdown と listener 停止のレースは起きうる）。
    #[test]
    fn apply_and_baseline_after_close_are_harmless() {
        let s = connected_store();
        s.close();
        s.apply(&onoff_event(6, true));
        s.baseline("desk_light", State::Off, s.generation());
        assert_eq!(
            primed(&s, "desk_light"),
            Some(State::Off),
            "格納自体は生きる"
        );
    }

    #[test]
    fn broadcasts_only_on_state_change() {
        let s = connected_store();
        let mut rx = s.subscribe();
        s.apply(&onoff_event(6, true));
        assert_eq!(
            rx.try_recv().unwrap(),
            StateEvent {
                device: "desk_light".into(),
                state: State::On,
                source: SOURCE_PUSH,
                stale: false,
            }
        );
        // 同じ値の再送では起こさない。
        s.apply(&onoff_event(6, true));
        assert!(rx.try_recv().is_err());
        // onoff を動かさない属性でも起こさない（明るさ・色が流れてきても静か）。
        s.apply(&PushEvent {
            node_id: 6,
            cluster: "levelcontrol".into(),
            attribute: "current-level".into(),
            value: json!(120),
        });
        assert!(rx.try_recv().is_err());
        // 変化したら起こす。
        s.apply(&onoff_event(6, false));
        assert_eq!(rx.try_recv().unwrap().state, State::Off);
    }

    #[test]
    fn baseline_broadcasts_with_read_source() {
        let s = connected_store();
        let mut rx = s.subscribe();
        s.baseline("desk_light", State::Off, s.generation());
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.source, SOURCE_READ);
        assert_eq!(ev.state, State::Off);
    }

    #[test]
    fn snapshot_omits_devices_without_a_baseline() {
        let s = connected_store();
        s.apply(&onoff_event(6, false));
        let snap = s.snapshot();
        assert_eq!(snap.len(), 1, "基準値のあるデバイスだけ: {snap:?}");
        assert_eq!(snap[0].device, "desk_light");
        assert_eq!(snap[0].state, State::Off);
        assert!(!snap[0].stale);
    }

    #[test]
    fn snapshot_is_empty_while_disconnected() {
        let s = connected_store();
        s.apply(&onoff_event(6, false));
        s.set_connected(false);
        assert!(s.snapshot().is_empty());
    }

    /// listener の断は 1〜2 回連続までは想定内（matd の restart / コンテナ再作成で
    /// 数秒消える）で info、3 回続いたら warn。
    #[test]
    fn listener_loss_is_alarming_only_when_it_repeats() {
        assert!(!listener_loss_is_alarming(1));
        assert!(!listener_loss_is_alarming(2));
        assert!(listener_loss_is_alarming(3));
        assert!(listener_loss_is_alarming(10));
    }

    #[tokio::test]
    async fn run_once_streams_lines_and_asks_for_rebaseline_after_a_grace() {
        let s = Arc::new(store());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let script = concat!(
            r#"printf '{"node_id":6,"cluster":"onoff","attribute":"on-off","value":true}\n'; "#,
            r#"printf 'garbage\n'; "#,
            r#"printf '{"node_id":6,"cluster":"onoff","attribute":"on-off","value":false}\n'; "#,
            r#"exec sleep 8"#,
        );
        let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        // 子は自分で終わらないので、依頼が届くまで待ってから見る。
        let listen = tokio::spawn({
            let s = s.clone();
            async move { run_once(&cmd, &s, &tx).await }
        });
        let got = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
        assert!(got.is_ok(), "猶予のあとに再ベースラインを依頼する");
        assert_eq!(
            primed(&s, "desk_light"),
            Some(State::Off),
            "壊れた行を挟んでもストリームは続く"
        );
        listen.abort();
    }

    #[tokio::test]
    async fn run_once_skips_rebaseline_when_the_child_dies_instantly() {
        // mat が古い / matd 不在で即死するケース。read を撒かない。
        let s = Arc::new(store());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cmd = vec!["sh".to_string(), "-c".to_string(), "exit 2".to_string()];
        let status = run_once(&cmd, &s, &tx).await.unwrap();
        assert_eq!(status.code(), Some(2));
        // 呼び出し側（run_listener）が世代を進める。
        s.set_connected(false);
        let got = tokio::time::timeout(Duration::from_secs(6), rx.recv()).await;
        assert!(
            got.is_err(),
            "即死した listener で再ベースラインを頼んではいけない"
        );
    }

    /// stdout に不正な UTF-8 が来ても run_once が速やかに戻ること。
    /// 戻らないと set_connected(false) に到達せず、「生きているのに何も
    /// 届かない」listener になる（鮮度モデルが存在しないと仮定する状態）。
    #[tokio::test]
    async fn run_once_returns_promptly_on_invalid_utf8_stdout() {
        let s = Arc::new(store());
        let (tx, _rx) = mpsc::unbounded_channel();
        // 不正なバイトを 1 行出したあと、自分では終わらない子。
        // exec で sh を置き換えるので子は sleep 自身になり、kill が孫を
        // 取り残さない（テストがプロセスを残して終わらないため）。
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"printf '\377\n'; exec sleep 60"#.to_string(),
        ];
        let started = Instant::now();
        let done = tokio::time::timeout(Duration::from_secs(20), run_once(&cmd, &s, &tx)).await;
        assert!(
            done.is_ok(),
            "run_once が戻らない（子を残したまま wait で固まっている）"
        );
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "戻るのが遅すぎる: {:?}",
            started.elapsed()
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
        assert_eq!(
            primed(&s, "desk_light"),
            None,
            "壊れた行で state を作らない"
        );
        // 壊れた行の後も正常な行は取り込める。
        ingest(
            &s,
            r#"{"timestamp":"t","node_id":6,"endpoint":1,"cluster":"onoff","attribute":"on-off","value":true,"priming":false,"recovered":false}"#,
        );
        assert_eq!(primed(&s, "desk_light"), Some(State::On));
    }

    #[test]
    fn ingest_takes_priming_and_recovered_events() {
        let s = connected_store();
        ingest(
            &s,
            r#"{"node_id":6,"cluster":"onoff","attribute":"on-off","value":true,"priming":true,"recovered":false}"#,
        );
        assert_eq!(
            primed(&s, "desk_light"),
            Some(State::On),
            "priming も recovered もその時点の実値を運ぶので受ける"
        );
    }
}
