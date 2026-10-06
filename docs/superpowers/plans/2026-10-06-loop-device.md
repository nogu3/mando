# loop デバイス（拍に合わせて照明を回す）実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** mando に `kind = "loop"` デバイスを足し、config のコマンド配列を BPM の拍に合わせて exec し続ける「クラブ」カードをスマホから始めて止められるようにする。

**Architecture:** ループは新規 `src/looper.rs` の tokio タスクが回す（開始時刻からの絶対スケジュール、遅れた拍は飛ばす）。各拍のコマンドは専用 `Executor` で timeout 有界に exec する（graph / mesh と同じ「専用 executor」の流儀）。状態は in-memory で `GET state` は exec しない。停止・自動停止・shutdown は同じ `on_stop` 配列を順に exec して照明を戻す。UI は `index.html` にカード 1 枚（BPM プリセット・タップテンポ・停止）。

**Tech Stack:** Rust (axum 0.7 / tokio 1 / serde / toml)、素の HTML+JS（`index.html` 焼き込み）。シェルスクリプト等の本体外コードは増やさない。

**Spec:** `docs/superpowers/specs/2026-10-06-loop-device-design.md`

## Global Constraints

- 本体以外のコード（シェルスクリプト等）は増やさない。拍の中身は config の配列。
- 新しい crate を足さない（SIGTERM 送信が不要になったので `libc` も不要）。
- `cargo test` / `cargo clippy -- -D warnings` / `cargo fmt --check` が通ること（各タスクの commit 前）。
- config の TOML キー: `kind = "loop"`, `bpms`, `max_minutes`（省略 60）, `bar_beats`（省略 4）, `beat`, `bar`, `on_stop`。shutter の `stop` とは別名。
- 値域: `bpms` 各 40〜200、`max_minutes` 1〜600、`bar_beats` 1〜16。`{fade}` は `beat` のみ可。
- API: `POST /api/devices/{name}/start` body `{"bpm":N}`（省略時 `bpms[0]`、不正は 400 `{"error":"invalid_bpm"}`）。`POST .../stop` と `GET .../state` は既存ルートを kind で分岐。state の形 `{"state":"running"|"stopped","bpm"?,"remaining_s"?}`。
- `{fade}` = 拍長 ms × 0.8 を 0.1 秒単位で四捨五入した整数、最小 1。
- 拍の exec 失敗はループを止めない。連続 10 拍失敗で warn 1 回。
- shutdown 時の `on_stop` は全 loop 分まとめて 5 秒で打ち切る。
- 設計原則（mando `CLAUDE.md`）: プロトコルを喋らない・コマンドは config テンプレ・成否を正直に。
- コミットメッセージ末尾に `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`。

## Review Focus

1. `start` を再生中に再度呼ぶ（タップで合わせ直し）: 古いタスクが確実に止まり、`on_stop` は走らず、新 BPM で即座に拍が始まること（Task 2 のテスト `restart_replaces_running_loop_without_on_stop`）。
2. 拍のコマンドが拍長より長くかかる（matd が詰まる）: 遅れを積まず未来の拍に揃うこと（Task 2 のテスト `slow_beat_skips_to_next_future_beat`）。
3. `bpm` に文字列・小数・範囲外・body 無しが来る: 範囲外は 400、body 無しは既定 BPM で開始（Task 3 のテスト `start_rejects_bpm_out_of_range` / `start_without_body_uses_first_preset`）。
4. shutdown が `on_stop` の exec で止まらない: 全 loop の `on_stop` を 5 秒で打ち切る（Task 2 のテスト `shutdown_bounds_on_stop_to_five_seconds`）。
5. loop 以外の kind に loop 用フィールドを書く／loop に light 用フィールドを書く: `--check` と起動で `ForbiddenField` になること（Task 1 のテスト `loop_fields_forbidden_on_other_kinds` / `loop_forbids_light_and_shutter_fields`）。

---

### Task 1: config — `Kind::Loop` とフィールド・検証

**Files:**
- Modify: `src/config.rs`（`Kind` enum / `Device` struct / `ConfigError` / `validate` / tests）
- Modify: `config.example.toml`（末尾に loop の例）

**Interfaces:**
- Produces: `Kind::Loop`、`Device` の pub フィールド `bpms: Vec<u32>`, `max_minutes: Option<u32>`, `bar_beats: Option<u32>`, `beat: Vec<Vec<String>>`, `bar: Vec<Vec<String>>`, `on_stop: Vec<Vec<String>>`、メソッド `Device::max_minutes(&self) -> u32`（省略 60）、`Device::bar_beats(&self) -> u32`（省略 4）、`Device::default_bpm(&self) -> u32`（`bpms[0]`）。`ConfigError::LoopValue { device, field, value }` と `ConfigError::FadePlaceholder { device, field }`。

- [ ] **Step 1: 失敗するテストを書く**（`src/config.rs` の `mod tests` 末尾。既存の `write_tmp` ヘルパを使う）

```rust
    const LOOP_OK: &str = r##"
        [[device]]
        name  = "club"
        kind  = "loop"
        bpms  = [100, 120, 128]
        beat = [
          ["mat", "group", "level", "--group", "g", "--percent", "100"],
          ["mat", "group", "level", "--group", "g", "--percent", "8", "--transition", "{fade}"],
        ]
        bar = [["mat", "group", "color", "--group", "g", "--name", "magenta"]]
        on_stop = [["mat", "group", "color-temp", "--group", "g", "--mireds", "370"]]
        "##;

    #[test]
    fn loop_minimal_config_loads_with_defaults() {
        let p = write_tmp("loop_ok", LOOP_OK);
        let cfg = Config::load(&p).unwrap();
        let d = cfg.find("club").unwrap();
        assert_eq!(d.kind, Kind::Loop);
        assert_eq!(d.bpms, vec![100, 120, 128]);
        assert_eq!(d.default_bpm(), 100);
        assert_eq!(d.max_minutes(), 60);
        assert_eq!(d.bar_beats(), 4);
        assert_eq!(d.beat.len(), 2);
        assert_eq!(d.bar.len(), 1);
        assert_eq!(d.on_stop.len(), 1);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn loop_requires_bpms_beat_and_on_stop() {
        for (tag, drop) in [("bpms", "bpms"), ("beat", "beat"), ("on_stop", "on_stop")] {
            // 該当行（配列の開始行から閉じ括弧まで）を消す。bpms は 1 行、beat/on_stop は複数行。
            let src: String = LOOP_OK
                .lines()
                .scan(false, |skipping, l| {
                    let t = l.trim_start();
                    if t.starts_with(&format!("{drop} ")) || t.starts_with(&format!("{drop}=")) {
                        *skipping = !t.ends_with(']') || t.matches('[').count() != t.matches(']').count();
                        return Some(None);
                    }
                    if *skipping {
                        if t == "]" { *skipping = false; }
                        return Some(None);
                    }
                    Some(Some(l))
                })
                .flatten()
                .collect::<Vec<_>>()
                .join("\n");
            let p = write_tmp(&format!("loop_missing_{tag}"), &src);
            assert!(
                matches!(Config::load(&p), Err(ConfigError::MissingCommand { field, .. }) if field == drop),
                "{drop} を消したら MissingCommand になるはず"
            );
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn loop_value_ranges() {
        for (tag, line, field) in [
            ("bpm_low", "bpms = [39]", "bpms"),
            ("bpm_high", "bpms = [201]", "bpms"),
            ("max_low", "bpms = [120]\nmax_minutes = 0", "max_minutes"),
            ("max_high", "bpms = [120]\nmax_minutes = 601", "max_minutes"),
            ("bar_low", "bpms = [120]\nbar_beats = 0", "bar_beats"),
            ("bar_high", "bpms = [120]\nbar_beats = 17", "bar_beats"),
        ] {
            let src = LOOP_OK.replace("bpms  = [100, 120, 128]", line);
            let p = write_tmp(&format!("loop_{tag}"), &src);
            assert!(
                matches!(Config::load(&p), Err(ConfigError::LoopValue { field: f, .. }) if f == field),
                "{tag}: {field} の値域エラーになるはず"
            );
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn loop_fade_only_in_beat() {
        for (tag, from, to, field) in [
            ("bar", r#"--name", "magenta"]"#, r#"--name", "{fade}"]"#, "bar"),
            ("on_stop", r#"--mireds", "370"]"#, r#"--mireds", "{fade}"]"#, "on_stop"),
        ] {
            let p = write_tmp(&format!("loop_fade_{tag}"), &LOOP_OK.replace(from, to));
            assert!(
                matches!(Config::load(&p), Err(ConfigError::FadePlaceholder { field: f, .. }) if f == field),
                "{tag}: {{fade}} は beat 以外で使えない"
            );
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn loop_rejects_empty_inner_command() {
        let p = write_tmp("loop_empty_inner", &LOOP_OK.replace(
            r#"bar = [["mat", "group", "color", "--group", "g", "--name", "magenta"]]"#,
            "bar = [[]]",
        ));
        assert!(matches!(Config::load(&p), Err(ConfigError::EmptyCommand(_))));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn loop_forbids_light_and_shutter_fields() {
        for (tag, extra, field) in [
            ("get_state", r#"get_state = ["mat", "read"]"#, "get_state"),
            ("on", r#"on = ["mat", "on"]"#, "on"),
            ("off", r#"off = ["mat", "off"]"#, "off"),
            ("open", r#"open = ["enl", "open"]"#, "open"),
            ("close", r#"close = ["enl", "close"]"#, "close"),
            ("stop", r#"stop = ["enl", "stop"]"#, "stop"),
            ("color", r#"color = ["mat", "{color}"]"#, "color"),
            ("brightness", r#"brightness = ["mat", "{brightness}"]"#, "brightness"),
            ("stateless", "stateless = true", "stateless"),
            ("face", r#"face = "light""#, "face"),
            ("members", r#"members = ["club"]"#, "members"),
            ("node_id", "node_id = 5", "node_id"),
            ("preset", "[[device.preset]]\nname = \"x\"\ncmd = [\"sh\"]", "preset"),
        ] {
            let src = if tag == "preset" {
                format!("{LOOP_OK}\n{extra}\n")
            } else {
                LOOP_OK.replace("kind  = \"loop\"", &format!("kind  = \"loop\"\n{extra}"))
            };
            let p = write_tmp(&format!("loop_forbid_{tag}"), &src);
            assert!(
                matches!(Config::load(&p), Err(ConfigError::ForbiddenField { field: f, .. }) if f == field),
                "{tag}: loop では {field} を書けない"
            );
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn loop_fields_forbidden_on_other_kinds() {
        for (tag, extra, field) in [
            ("bpms", "bpms = [120]", "bpms"),
            ("max_minutes", "max_minutes = 10", "max_minutes"),
            ("bar_beats", "bar_beats = 4", "bar_beats"),
            ("beat", r#"beat = [["sh"]]"#, "beat"),
            ("bar", r#"bar = [["sh"]]"#, "bar"),
            ("on_stop", r#"on_stop = [["sh"]]"#, "on_stop"),
        ] {
            let src = format!(
                r##"
                [[device]]
                name = "l"
                kind = "light"
                get_state = ["mat", "read"]
                on  = ["mat", "on"]
                off = ["mat", "off"]
                {extra}
                "##
            );
            let p = write_tmp(&format!("light_loopfield_{tag}"), &src);
            assert!(
                matches!(Config::load(&p), Err(ConfigError::ForbiddenField { field: f, .. }) if f == field),
                "{tag}: light に {field} は書けない"
            );
            std::fs::remove_file(p).ok();
        }
    }
```

- [ ] **Step 2: 失敗を確認**

Run: `cargo test --bin mando config::tests::loop_ 2>&1 | tail -20`
Expected: コンパイルエラー（`Kind::Loop` / `bpms` / `LoopValue` が無い）。

- [ ] **Step 3: 実装**

`Kind` に `Loop` を追加:

```rust
pub enum Kind {
    #[default]
    Shutter,
    Light,
    Switch,
    /// config のコマンド配列を BPM の拍に合わせて exec し続ける（クラブ照明など）。
    /// 状態は in-memory（running / stopped）で、get_state は持たない。
    Loop,
}
```

`Device` の `lane` の直後にフィールドを追加:

```rust
    /// loop: UI の BPM プリセット（先頭が既定）。loop 必須・他 kind 不可。各 40〜200。
    #[serde(default)]
    pub bpms: Vec<u32>,
    /// loop: 自動停止までの分（省略 60）。1〜600。
    #[serde(default)]
    pub max_minutes: Option<u32>,
    /// loop: 1 小節の拍数（省略 4）。1〜16。
    #[serde(default)]
    pub bar_beats: Option<u32>,
    /// loop: 毎拍、順に exec するコマンド配列（必須）。`{fade}` は拍長の 80% を 0.1 秒単位にした値。
    #[serde(default)]
    pub beat: Vec<Vec<String>>,
    /// loop: 小節の頭に beat より先に exec するコマンド配列。小節ごとに順繰り（任意）。
    #[serde(default)]
    pub bar: Vec<Vec<String>>,
    /// loop: 停止・自動停止・shutdown 時に順に exec するコマンド配列（必須）。
    /// shutter の `stop`（1 コマンド）と混ざらないよう別名。
    #[serde(default)]
    pub on_stop: Vec<Vec<String>>,
```

`impl Device` に追加:

```rust
    /// loop: 自動停止までの分（省略 60）。
    pub fn max_minutes(&self) -> u32 {
        self.max_minutes.unwrap_or(60)
    }

    /// loop: 1 小節の拍数（省略 4）。
    pub fn bar_beats(&self) -> u32 {
        self.bar_beats.unwrap_or(4)
    }

    /// loop: 既定 BPM（`bpms[0]`。validate で 1 個以上を保証済み）。
    pub fn default_bpm(&self) -> u32 {
        self.bpms.first().copied().unwrap_or(120)
    }
```

`ConfigError` に追加（`EmptyRulesCommand` の前）と Display:

```rust
    /// loop の数値フィールドが値域外。
    LoopValue {
        device: String,
        field: &'static str,
        value: u64,
    },
    /// `{fade}` を beat 以外に書いた。
    FadePlaceholder {
        device: String,
        field: &'static str,
    },
```

```rust
            ConfigError::LoopValue { device, field, value } => {
                write!(f, "device {device}: {field} の値 {value} が範囲外")
            }
            ConfigError::FadePlaceholder { device, field } => {
                write!(f, "device {device}: {{fade}} は beat でしか使えない（{field} にある）")
            }
```

`validate` の get_state 事前検査を loop で飛ばす:

```rust
            } else if d.kind != Kind::Loop && d.get_state.is_empty() {
                return Err(ConfigError::EmptyCommand(d.name.clone()));
            }
```

`match d.kind` に `Kind::Loop` の腕を追加（`Kind::Switch` の後）:

```rust
                Kind::Loop => {
                    forbid(&d.name, "open", &d.open)?;
                    forbid(&d.name, "close", &d.close)?;
                    forbid(&d.name, "stop", &d.stop)?;
                    forbid(&d.name, "on", &d.on)?;
                    forbid(&d.name, "off", &d.off)?;
                    forbid(&d.name, "color", &d.color)?;
                    forbid(&d.name, "brightness", &d.brightness)?;
                    let forbidden_field = |field: &'static str| ConfigError::ForbiddenField {
                        device: d.name.clone(),
                        field,
                    };
                    if !d.get_state.is_empty() {
                        return Err(forbidden_field("get_state"));
                    }
                    if d.face.is_some() {
                        return Err(forbidden_field("face"));
                    }
                    if !d.presets.is_empty() {
                        return Err(forbidden_field("preset"));
                    }
                    if !d.members.is_empty() {
                        return Err(forbidden_field("members"));
                    }
                    if d.node_id.is_some() {
                        return Err(forbidden_field("node_id"));
                    }
                    let loop_value = |field: &'static str, value: u64| ConfigError::LoopValue {
                        device: d.name.clone(),
                        field,
                        value,
                    };
                    if d.bpms.is_empty() {
                        return Err(ConfigError::MissingCommand {
                            device: d.name.clone(),
                            field: "bpms",
                        });
                    }
                    for &b in &d.bpms {
                        if !(40..=200).contains(&b) {
                            return Err(loop_value("bpms", b as u64));
                        }
                    }
                    let mm = d.max_minutes();
                    if !(1..=600).contains(&mm) {
                        return Err(loop_value("max_minutes", mm as u64));
                    }
                    let bb = d.bar_beats();
                    if !(1..=16).contains(&bb) {
                        return Err(loop_value("bar_beats", bb as u64));
                    }
                    // コマンド配列: beat / on_stop は 1 個以上、各コマンドは空不可。
                    // {fade} は beat のみ。
                    let has_fade = |cmds: &[Vec<String>]| {
                        cmds.iter().flatten().any(|s| s.contains("{fade}"))
                    };
                    for (field, cmds, required) in [
                        ("beat", &d.beat, true),
                        ("bar", &d.bar, false),
                        ("on_stop", &d.on_stop, true),
                    ] {
                        if required && cmds.is_empty() {
                            return Err(ConfigError::MissingCommand {
                                device: d.name.clone(),
                                field,
                            });
                        }
                        if cmds.iter().any(|c| c.is_empty()) {
                            return Err(ConfigError::EmptyCommand(d.name.clone()));
                        }
                        if field != "beat" && has_fade(cmds) {
                            return Err(ConfigError::FadePlaceholder {
                                device: d.name.clone(),
                                field,
                            });
                        }
                    }
                }
```

`match d.kind { ... }` の直後（同じ `for d in &self.devices` の中）に loop 用フィールドの他 kind 禁止:

```rust
            if d.kind != Kind::Loop {
                let forbidden_field = |field: &'static str| ConfigError::ForbiddenField {
                    device: d.name.clone(),
                    field,
                };
                if !d.bpms.is_empty() {
                    return Err(forbidden_field("bpms"));
                }
                if d.max_minutes.is_some() {
                    return Err(forbidden_field("max_minutes"));
                }
                if d.bar_beats.is_some() {
                    return Err(forbidden_field("bar_beats"));
                }
                if !d.beat.is_empty() {
                    return Err(forbidden_field("beat"));
                }
                if !d.bar.is_empty() {
                    return Err(forbidden_field("bar"));
                }
                if !d.on_stop.is_empty() {
                    return Err(forbidden_field("on_stop"));
                }
            }
```

既存テストで `Device { ... }` を直接組み立てている箇所（`lane: None, stateless: false,` のある構造体リテラル）があれば、新フィールド `bpms: vec![], max_minutes: None, bar_beats: None, beat: vec![], bar: vec![], on_stop: vec![],` を足す。

`config.example.toml` 末尾に追加:

```toml
# ── loop（拍に合わせてコマンドを回す。クラブ照明など）──────────────
# 状態は in-memory（running / stopped）。get_state は持たない。
# beat: 毎拍、順に exec。{fade} は拍長の 80% を 0.1 秒単位にした整数（120 BPM で 4）。
# bar: 小節の頭に beat より先に exec。小節ごとに順繰り。
# on_stop: 停止・自動停止（max_minutes）・mando 終了時に順に exec（元の明かりに戻す）。
# [[device]]
# name  = "club_living"
# alias = "クラブ（リビング）"
# kind  = "loop"
# bpms  = [100, 120, 128]   # UI のプリセット。先頭が既定
# max_minutes = 60          # 省略時 60
# bar_beats = 4             # 省略時 4
# beat = [
#   ["mat", "group", "level", "--group", "living_lights", "--percent", "100"],
#   ["mat", "group", "level", "--group", "living_lights", "--percent", "8", "--transition", "{fade}"],
# ]
# bar = [
#   ["mat", "group", "color", "--group", "living_lights", "--name", "magenta"],
#   ["mat", "group", "color", "--group", "living_lights", "--name", "blue"],
#   ["mat", "group", "color", "--group", "living_lights", "--name", "purple"],
#   ["mat", "group", "color", "--group", "living_lights", "--name", "cyan"],
# ]
# on_stop = [
#   ["mat", "group", "color-temp", "--group", "living_lights", "--mireds", "370"],
#   ["mat", "group", "level", "--group", "living_lights", "--percent", "100"],
# ]
```

- [ ] **Step 4: テスト・lint**

Run: `cargo test --bin mando config:: 2>&1 | tail -5 && cargo clippy -- -D warnings && cargo fmt --check`
Expected: 全 config テスト PASS（既存含む）、clippy / fmt エラーなし。`main.rs` の `match d.kind` に網羅性エラーが出たら Task 3 で直すので、ここでは `Kind::Loop => ...` を既存の最も近い腕と同じ扱いにせず、**`_ => {}` を足さない**。網羅性エラーが出る箇所は `Kind::Loop` の腕を明示して「unsupported」側に倒す（`device_cmd` は `None`、`normalize` 分岐は `Kind::Switch` と同じ扱い）。

- [ ] **Step 5: Commit**

```bash
git add src/config.rs config.example.toml src/main.rs
git commit -m "feat(config): kind=loop — bpms/beat/bar/on_stop と検証

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `looper.rs` — ループタスク本体

**Files:**
- Create: `src/looper.rs`
- Modify: `src/main.rs`（`mod looper;` の 1 行だけ。使うのは Task 3）

**Interfaces:**
- Consumes: `crate::exec::{Executor, ExecOutcome}`。
- Produces:
  - `pub struct LoopSpec { pub lane: String, pub bar_beats: u32, pub beat: Vec<Vec<String>>, pub bar: Vec<Vec<String>>, pub on_stop: Vec<Vec<String>> }`
  - `pub struct LoopStatus { pub state: &'static str, pub bpm: Option<u32>, pub remaining_s: Option<u64> }`（`Serialize`、None は省略）
  - `pub struct Looper`（`Looper::new(timeout: Duration) -> Looper`。exec は内部の専用 `Executor`）
  - `pub fn Looper::status(&self, name: &str) -> LoopStatus`
  - `pub fn Looper::start(self: &Arc<Self>, name: &str, spec: LoopSpec, bpm: u32, max: Duration) -> LoopStatus`（running なら止めて置き換え、`on_stop` は走らせない）
  - `pub async fn Looper::stop(&self, name: &str, spec: &LoopSpec) -> LoopStatus`（停止済みでも `on_stop` を exec）
  - `pub async fn Looper::shutdown(&self, specs: &[(String, LoopSpec)])`（全 loop を止め、`on_stop` を全体 5 秒で打ち切り）
  - `pub fn fade_ds(bpm: u32) -> u64`（テスト用に pub）

- [ ] **Step 1: 失敗するテストを書く**（`src/looper.rs` を新規作成し、まず tests だけ書く。`sh` でコマンドを偽装し、ファイルに行を追記して観測する）

```rust
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

    fn looper() -> Arc<Looper> {
        Arc::new(Looper::new(Duration::from_secs(5)))
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
        let expect = ["bar0", "beat", "fade=2", "beat", "fade=2", "bar1", "beat", "fade=2", "beat", "fade=2"];
        assert!(got.len() >= expect.len(), "1 秒で 4 拍分は走るはず: {got:?}");
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
        assert!(!got.iter().any(|l| l == "stop"), "合わせ直しでは on_stop を走らせない: {got:?}");
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
        assert!(gap_ms >= 850, "遅れを積まず拍 3（900 ms）に揃うはず: gap={gap_ms}ms");
        std::fs::remove_file(p).ok();
    }

    #[tokio::test]
    async fn failing_beats_do_not_stop_the_loop() {
        let p = tmp("fail");
        let l = looper();
        let sp = LoopSpec {
            lane: "club".into(),
            bar_beats: 4,
            beat: vec![vec!["sh".into(), "-c".into(), format!("echo x >> {p}; exit 1")]],
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
        l.shutdown(&[("a".into(), spec(&p1)), ("b".into(), spec(&p2))]).await;
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
        assert!(took < Duration::from_secs(7), "shutdown は 5 秒で打ち切る: {took:?}");
        std::fs::remove_file(p).ok();
    }
}
```

- [ ] **Step 2: `main.rs` に `mod looper;` を足し、失敗を確認**

`src/main.rs` の `mod exec;` の後に `mod looper;` を追加。

Run: `cargo test --bin mando looper:: 2>&1 | tail -20`
Expected: コンパイルエラー（`Looper` 等が未定義）。

- [ ] **Step 3: 実装**（`src/looper.rs` の tests の上に書く）

```rust
//! loop デバイス: config のコマンド配列を BPM の拍に合わせて exec し続ける。
//!
//! mando が知るのは「config の配列をタイミング付きで exec する」ことだけ。
//! 照明の群・色・パルスの形は config（設計原則 1・2）。
//!
//! - 拍は開始時刻からの絶対スケジュール（ドリフトしない）。処理が次の拍を越えたら
//!   越えた分は飛ばして未来の拍に揃える（遅れを積まない）。
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
}

/// `{fade}`: 拍長 ms × 0.8 を 0.1 秒単位で四捨五入（最小 1）。
pub fn fade_ds(bpm: u32) -> u64 {
    let beat_ms = 60_000 / bpm as u64;
    ((beat_ms * 8 + 500) / 1000).max(1)
}

impl Looper {
    pub fn new(timeout: Duration) -> Self {
        Looper {
            executor: Executor::new(),
            timeout,
            runs: Mutex::new(HashMap::new()),
            next_gen: AtomicU64::new(1),
        }
    }

    pub fn status(&self, name: &str) -> LoopStatus {
        let runs = self.runs.lock().expect("runs poisoned");
        match runs.get(name) {
            Some(r) => LoopStatus {
                state: "running",
                bpm: Some(r.bpm),
                remaining_s: Some(r.deadline.saturating_duration_since(Instant::now()).as_secs()),
            },
            None => LoopStatus::stopped(),
        }
    }

    /// 起動。running なら古いタスクを止めて置き換える（on_stop は走らせない —
    /// 直後に新ループが上書きする）。
    pub fn start(self: &Arc<Self>, name: &str, spec: LoopSpec, bpm: u32, max: Duration) -> LoopStatus {
        if let Some(old) = self.take(name) {
            abort_run(old);
        }
        let (cancel, rx) = watch::channel(false);
        let now = Instant::now();
        let deadline = now + max;
        let gen = self.next_gen.fetch_add(1, Ordering::Relaxed);
        let task = tokio::spawn(run_loop(
            self.clone(),
            name.to_string(),
            spec,
            bpm,
            deadline,
            rx,
            gen,
        ));
        self.runs.lock().expect("runs poisoned").insert(
            name.to_string(),
            LoopRun {
                bpm,
                deadline,
                cancel,
                task,
                gen,
            },
        );
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
        let work = async {
            for (name, spec) in stopped {
                self.run_on_stop(name, spec).await;
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

    async fn run_on_stop(&self, name: &str, spec: &LoopSpec) {
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
        if !spec.bar.is_empty() && i % bar_beats == 0 {
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
        // 次の拍。処理が遅れて次の拍を越えていたら、越えた分は飛ばす。
        let elapsed_ms = start.elapsed().as_millis() as u64;
        i = (i + 1).max(elapsed_ms / beat_ms + 1);
    }
    // 自動停止: 自分の世代のエントリだけ外し、on_stop を走らせる。
    if looper.take_if_gen(&name, gen) {
        tracing::info!(device = %name, "loop 自動停止（max_minutes）");
        looper.run_on_stop(&name, &spec).await;
    }
}
```

- [ ] **Step 4: テスト・lint**

Run: `cargo test --bin mando looper:: 2>&1 | tail -20 && cargo clippy -- -D warnings && cargo fmt --check`
Expected: looper の 11 テストすべて PASS（タイミング依存のテストは `--test-threads=1` でも PASS すること: `cargo test --bin mando looper:: -- --test-threads=1`）。`mod looper` が未使用警告になる場合は `#[allow(dead_code)]` を付けず、Task 3 まで `cargo clippy` の dead_code は `-A dead_code` で逃がさない — 代わりに `main.rs` で `pub use` せず、**Task 3 で使うまでは `mod looper;` に `#[allow(dead_code)]` を一時的に付け、Task 3 で外す**。

- [ ] **Step 5: Commit**

```bash
git add src/looper.rs src/main.rs
git commit -m "feat(looper): 拍スケジュールでコマンド配列を回すループタスク

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `main.rs` — API（start / stop / state / list）と shutdown 連携

**Files:**
- Modify: `src/main.rs`（`App` / `main` / `router` / `list_devices` / `get_state` / `device_op` / `run_shutdown` / tests）
- Modify: `CLAUDE.md`（「やること（安定ミニ API）」に `start` を 1 行追加）

**Interfaces:**
- Consumes: Task 1 の `Kind::Loop` と `Device` フィールド、Task 2 の `looper::{Looper, LoopSpec, LoopStatus}`。
- Produces: `POST /api/devices/:name/start`、`DeviceInfo` に `bpms: Vec<u32>`（空なら省略）と `max_minutes: Option<u32>`（None なら省略）、`fn loop_spec(device: &Device) -> LoopSpec`。

- [ ] **Step 1: 失敗するテストを書く**（`src/main.rs` の `mod tests` 末尾。`app_from` / `call_on` を使う。JSON body 付き POST 用に `call_json` を足す）

```rust
    /// JSON body 付きで叩く（start の bpm 指定用）。
    async fn call_json(app: Shared, method: &str, path: &str, body: &str) -> (axum::http::StatusCode, Value) {
        let res = router(app)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
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

    fn loop_app(log: &str) -> Shared {
        app_from(&format!(
            r##"
            [[device]]
            name  = "club"
            alias = "クラブ"
            kind  = "loop"
            bpms  = [100, 120, 128]
            max_minutes = 1
            bar_beats = 2
            beat = [["sh", "-c", "echo beat >> {log}"]]
            bar  = [["sh", "-c", "echo bar >> {log}"]]
            on_stop = [["sh", "-c", "echo stop >> {log}"]]
            [[device]]
            name = "shutter"
            get_state = ["sh", "-c", "printf '{{\"properties\":[{{\"name\":\"open_close_state\",\"value\":\"open\"}}]}}'"]
            open  = ["sh", "-c", "printf '{{}}'"]
            close = ["sh", "-c", "printf '{{}}'"]
            "##
        ))
    }

    #[tokio::test]
    async fn list_devices_exposes_loop_bpms_and_max_minutes() {
        let app = loop_app(&tmp_counter("loop_list"));
        let (st, v) = call_on(app, "GET", "/api/devices").await;
        assert_eq!(st, 200);
        let club = v.as_array().unwrap().iter().find(|d| d["name"] == "club").unwrap();
        assert_eq!(club["kind"], "loop");
        assert_eq!(club["label"], "クラブ");
        assert_eq!(club["bpms"], serde_json::json!([100, 120, 128]));
        assert_eq!(club["max_minutes"], 1);
        let shutter = v.as_array().unwrap().iter().find(|d| d["name"] == "shutter").unwrap();
        assert!(shutter.get("bpms").is_none(), "loop 以外に bpms は出さない");
        assert!(shutter.get("max_minutes").is_none());
    }

    #[tokio::test]
    async fn loop_state_is_stopped_without_exec() {
        let log = tmp_counter("loop_state");
        let app = loop_app(&log);
        let (st, v) = call_on(app, "GET", "/api/devices/club/state").await;
        assert_eq!(st, 200);
        assert_eq!(v, serde_json::json!({"state": "stopped"}));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "", "state は exec しない");
    }

    #[tokio::test]
    async fn start_then_state_then_stop() {
        let log = tmp_counter("loop_cycle");
        let app = loop_app(&log);
        let (st, v) = call_json(app.clone(), "POST", "/api/devices/club/start", r#"{"bpm":128}"#).await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["state"], "running");
        assert_eq!(v["bpm"], 128);
        assert!(v["remaining_s"].as_u64().unwrap() <= 60);
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let (_, v) = call_on(app.clone(), "GET", "/api/devices/club/state").await;
        assert_eq!(v["state"], "running");
        assert_eq!(v["bpm"], 128);
        let (st, v) = call_on(app.clone(), "POST", "/api/devices/club/stop").await;
        assert_eq!(st, 200);
        assert_eq!(v, serde_json::json!({"state": "stopped"}));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("bar\nbeat\n"), "{text:?}");
        assert!(text.ends_with("stop\n"), "{text:?}");
        let (_, v) = call_on(app, "GET", "/api/devices/club/state").await;
        assert_eq!(v["state"], "stopped");
    }

    #[tokio::test]
    async fn start_without_body_uses_first_preset() {
        let log = tmp_counter("loop_default");
        let app = loop_app(&log);
        let (st, v) = call_on(app.clone(), "POST", "/api/devices/club/start").await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["bpm"], 100);
        call_on(app, "POST", "/api/devices/club/stop").await;
    }

    #[tokio::test]
    async fn start_rejects_bpm_out_of_range() {
        let log = tmp_counter("loop_badbpm");
        let app = loop_app(&log);
        for body in [r#"{"bpm":39}"#, r#"{"bpm":201}"#, r#"{"bpm":"fast"}"#, r#"{"bpm":120.5}"#] {
            let (st, v) = call_json(app.clone(), "POST", "/api/devices/club/start", body).await;
            assert_eq!(st, 400, "{body}");
            assert_eq!(v["error"], "invalid_bpm", "{body}");
        }
        let (_, v) = call_on(app, "GET", "/api/devices/club/state").await;
        assert_eq!(v["state"], "stopped");
    }

    #[tokio::test]
    async fn start_on_non_loop_is_unsupported() {
        let log = tmp_counter("loop_nonloop");
        let app = loop_app(&log);
        let (st, _) = call_on(app.clone(), "POST", "/api/devices/shutter/start").await;
        assert_eq!(st, 404);
        let (st, _) = call_on(app, "POST", "/api/devices/nope/start").await;
        assert_eq!(st, 404);
    }

    #[tokio::test]
    async fn loop_rejects_open_close_on_off() {
        let log = tmp_counter("loop_ops");
        let app = loop_app(&log);
        for op in ["open", "close", "on", "off"] {
            let (st, v) = call_on(app.clone(), "POST", &format!("/api/devices/club/{op}")).await;
            assert_eq!(st, 404, "{op}");
            assert_eq!(v["error"], "unsupported operation", "{op}");
        }
    }

    #[tokio::test]
    async fn shutdown_runs_loop_on_stop() {
        let log = tmp_counter("loop_shutdown");
        let app = loop_app(&log);
        call_on(app.clone(), "POST", "/api/devices/club/start").await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        run_shutdown(std::future::ready(()), None, vec![], app.clone()).await;
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.ends_with("stop\n"), "{text:?}");
        let (_, v) = call_on(app, "GET", "/api/devices/club/state").await;
        assert_eq!(v["state"], "stopped");
    }
```

- [ ] **Step 2: 失敗を確認**

Run: `cargo test --bin mando tests::loop_ tests::start_ tests::list_devices_exposes_loop tests::shutdown_runs_loop 2>&1 | tail -20`
Expected: コンパイルエラー（`run_shutdown` の引数数・`bpms` が無い等）。

- [ ] **Step 3: 実装**

`App` にフィールド追加（`push` の後）:

```rust
    /// loop デバイスのランタイム（専用 executor・in-memory 状態）。
    looper: Arc<looper::Looper>,
```

`main` の `App` 構築に `looper: Arc::new(looper::Looper::new(std::time::Duration::from_millis(config.exec.timeout_ms))),` を追加（`config` を move する前に `timeout_ms` を読む。`let exec_timeout_ms = config.exec.timeout_ms;` を `App` 構築の前に置く）。tests の全 `App { ... }` 構築箇所（`grep -n "App {" src/main.rs`、9 箇所）にも `looper: Arc::new(looper::Looper::new(std::time::Duration::from_secs(15))),` を追加。Task 2 で `mod looper;` に付けた `#[allow(dead_code)]` を外す。

`DeviceInfo` に追加:

```rust
    /// loop の BPM プリセット（loop 以外は省略）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bpms: Vec<u32>,
    /// loop の自動停止までの分（loop 以外は省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    max_minutes: Option<u32>,
```

`list_devices` の map に:

```rust
            bpms: d.bpms.clone(),
            max_minutes: (d.kind == Kind::Loop).then(|| d.max_minutes()),
```

`loop_spec` ヘルパ（`device_cmd` の近く）:

```rust
/// loop の Device から looper に渡す中身を組む（Device → LoopSpec の写しはここだけ）。
fn loop_spec(device: &Device) -> looper::LoopSpec {
    looper::LoopSpec {
        lane: device.exec_lane().to_string(),
        bar_beats: device.bar_beats(),
        beat: device.beat.clone(),
        bar: device.bar.clone(),
        on_stop: device.on_stop.clone(),
    }
}
```

`router` に `.route("/api/devices/:name/start", post(start_device))` を追加（`stop` の次の行）。

`get_state` を kind で分岐:

```rust
async fn get_state(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.config.find(&name) {
        // loop は in-memory 即答（exec しない）。
        Some(device) if device.kind == Kind::Loop => Json(app.looper.status(&name)).into_response(),
        Some(device) => Json(cached_state(&app, device).await).into_response(),
        None => not_found(&name),
    }
}
```

`device_op` の先頭（`find` の直後）に loop の stop 分岐:

```rust
    if device.kind == Kind::Loop {
        if let Op::Stop = op {
            return Json(app.looper.stop(&name, &loop_spec(device)).await).into_response();
        }
        // open/close/on/off は device_cmd が None を返し「unsupported operation」に落ちる。
    }
```

`start_device` ハンドラ（`stop_device` の後）:

```rust
#[derive(Deserialize)]
struct StartBody {
    bpm: Option<serde_json::Value>,
}

/// loop を起動（running なら新 bpm で置き換え）。body 無し／`bpm` 無しは `bpms[0]`。
async fn start_device(
    State(app): State<Shared>,
    Path(name): Path<String>,
    body: Option<Json<StartBody>>,
) -> Response {
    let Some(device) = app.config.find(&name) else {
        return not_found(&name);
    };
    if device.kind != Kind::Loop {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            format!(
                "{{\"error\":\"unsupported operation\",\"name\":{}}}",
                json_str(&name)
            ),
        )
            .into_response();
    }
    // bpm: 省略時は既定。整数 40〜200 以外（文字列・小数・範囲外）は 400。
    let bpm = match body.and_then(|Json(b)| b.bpm) {
        None | Some(Value::Null) => device.default_bpm(),
        Some(v) => match v.as_u64() {
            Some(n) if (40..=200).contains(&n) => n as u32,
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    "{\"error\":\"invalid_bpm\"}",
                )
                    .into_response()
            }
        },
    };
    let max = std::time::Duration::from_secs(device.max_minutes() as u64 * 60);
    Json(app.looper.start(&name, loop_spec(device), bpm, max)).into_response()
}
```

`run_shutdown` に `app: Shared` を足し、loop を先に止める（照明を戻してから push を落とす）:

```rust
async fn run_shutdown(
    signal: impl std::future::Future<Output = ()>,
    store: Option<Arc<push::PushStore>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    app: Shared,
) {
    signal.await;
    tracing::info!("shutdown");
    // loop を先に止めて照明を戻す（on_stop は合計 5 秒で打ち切り）。
    let specs: Vec<(String, looper::LoopSpec)> = app
        .config
        .devices
        .iter()
        .filter(|d| d.kind == Kind::Loop)
        .map(|d| (d.name.clone(), loop_spec(d)))
        .collect();
    app.looper.shutdown(&specs).await;
    if let Some(store) = store {
        store.close();
    }
    ...（以下既存のまま）
```

`main` の呼び出しを `run_shutdown(wait_for_signal(), store, push_tasks, app.clone())` に変更（`app` は `start_push(app, s)` で move されているので、その前に `let app_for_shutdown = app.clone();` を取る）。既存テスト `run_shutdown_closes_store_and_stops_push_tasks` の呼び出しにも第 4 引数（そのテストで作っている `App`、無ければ `test_app()`）を足す。

Task 1 で `match d.kind` の網羅性のために足した `Kind::Loop` の腕があれば、意図どおり（`device_cmd` は `None`）か確認する。

`CLAUDE.md` の「やること（安定ミニ API）」に追記:

```markdown
- `POST /api/devices/{name}/start` — loop を起動（body `{"bpm": N}` 任意。running なら新 bpm で置き換え）。loop の `stop` は `on_stop` を exec、`state` は in-memory 即答 `{ "state": "running|stopped", "bpm"?, "remaining_s"? }`（`docs/superpowers/specs/2026-10-06-loop-device-design.md`）
```

- [ ] **Step 4: テスト・lint**

Run: `cargo test 2>&1 | tail -5 && cargo clippy -- -D warnings && cargo fmt --check`
Expected: 全テスト PASS（既存含む）。

- [ ] **Step 5: Commit**

```bash
git add src/main.rs CLAUDE.md
git commit -m "feat(api): loop の start/stop/state と shutdown 連携

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `index.html` — クラブカード（プリセット・タップ・停止）

**Files:**
- Modify: `index.html`（CSS に `.loopcard` 一式、JS に `buildLoopCard` / `renderLoop` / `loopStart` / タップ処理、`boot` のセクション追加）

**Interfaces:**
- Consumes: `GET /api/devices` の `kind: "loop"`, `label`, `bpms`, `max_minutes`。`POST /api/devices/{name}/start` body `{bpm}`、`POST .../stop`、`GET .../state`（`{state, bpm?, remaining_s?}`）。既存の `api(method, path, body)`、`sectionHeading(text)`、`button.act` のスタイル。
- Produces: loop カードは `cards` Map に**登録しない**（`renderState` / ポーリングは light・shutter 用）。自前で再生中のみ 5 秒ポーリング。

- [ ] **Step 1: CSS を追加**（`.tile .onoff button.act { ... }` の行の直後）

```css
  /* ── loop カード（クラブ照明）。再生中は見出しの状態が光る ── */
  .loopcard { background: var(--card); border: 1px solid var(--line); border-radius: 16px; padding: 12px 14px; margin: 0 0 12px; }
  .loopcard .lhead { display: flex; justify-content: space-between; align-items: baseline; gap: 8px; }
  .loopcard .lname { font-size: 15px; font-weight: 700; }
  .loopcard .lstate { font-size: 12px; color: var(--muted); }
  .loopcard.playing .lstate { color: #ff6ad5; font-weight: 600; }
  .loopcard .lctl { display: flex; gap: 8px; margin-top: 10px; }
  .loopcard .lctl button.act { flex: 1; min-height: 46px; font-size: 15px; border-radius: 12px; }
  .loopcard .lctl button.act.tap { flex: 1.4; }
  .loopcard .lctl button.act.stop { flex: 1; }
  .loopcard.playing button.act.bpm.cur { outline: 2px solid #ff6ad5; }
  .loopcard .lmsg { font-size: 11px; color: var(--muted); min-height: 16px; margin-top: 6px; }
  .loopcard .lmsg.error { color: var(--warn); }
```

既存の `--card` / `--line` / `--muted` / `--warn` 変数名が無い場合は、`.tile` と `.group` の CSS が使っている背景・罫線・文字色の変数名に合わせる（`grep -n "^\s*--" index.html` で確認）。

- [ ] **Step 2: JS を追加**（`buildStatelessSwitchTile` の直後）

```js
/* ── loop カード（クラブ照明）。状態は in-memory 即答なので cards には登録せず、
   再生中だけ 5 秒ごとに自前で state を取る。 ── */
const LOOP_POLL_MS = 5000;
const TAP_SETTLE_MS = 1500;   // 最後のタップからこれだけ空いたら確定して start
const TAP_RESET_MS  = 2000;   // 間隔がこれを超えたら叩き直し
const TAP_KEEP = 9;           // 直近 9 回（= 8 間隔）

function buildLoopCard(dev) {
  const el = document.createElement("div");
  el.className = "loopcard";
  el.innerHTML = `
    <div class="lhead"><span class="lname"></span><span class="lstate">停止中</span></div>
    <div class="lctl"></div>
    <div class="lmsg"></div>
  `;
  el.querySelector(".lname").textContent = dev.label;
  const ctl = el.querySelector(".lctl");
  const card = {
    name: dev.name,
    rootEl: el,
    stateEl: el.querySelector(".lstate"),
    msgEl: el.querySelector(".lmsg"),
    bpmBtns: [],
    tapBtn: null,
    timer: null,
    taps: [],
    tapTimer: null,
  };
  for (const bpm of dev.bpms || []) {
    const b = document.createElement("button");
    b.className = "act bpm";
    b.type = "button";
    b.dataset.bpm = String(bpm);
    b.textContent = String(bpm);
    b.addEventListener("click", () => loopStart(card, bpm));
    ctl.appendChild(b);
    card.bpmBtns.push(b);
  }
  const tap = document.createElement("button");
  tap.className = "act tap";
  tap.type = "button";
  tap.textContent = "タップ";
  tap.addEventListener("click", () => loopTap(card));
  ctl.appendChild(tap);
  card.tapBtn = tap;
  const stop = document.createElement("button");
  stop.className = "act stop";
  stop.type = "button";
  stop.textContent = "停止";
  stop.addEventListener("click", () => loopStop(card));
  ctl.appendChild(stop);
  loopRefresh(card);
  return el;
}

function renderLoop(card, st) {
  const playing = st && st.state === "running";
  card.rootEl.classList.toggle("playing", playing);
  if (playing) {
    const min = Math.max(1, Math.ceil((st.remaining_s || 0) / 60));
    card.stateEl.textContent = `${st.bpm} BPM で再生中 ・ 残り ${min} 分`;
    for (const b of card.bpmBtns) b.classList.toggle("cur", Number(b.dataset.bpm) === st.bpm);
    if (!card.timer) card.timer = setInterval(() => loopRefresh(card), LOOP_POLL_MS);
  } else {
    card.stateEl.textContent = "停止中";
    for (const b of card.bpmBtns) b.classList.remove("cur");
    if (card.timer) { clearInterval(card.timer); card.timer = null; }
  }
}

async function loopRefresh(card) {
  try {
    renderLoop(card, await api("GET", `/api/devices/${encodeURIComponent(card.name)}/state`));
  } catch (e) {
    // 取れなくても表示は前回のまま。再生中なら次の周期で再試行する。
  }
}

function loopMsg(card, text, isError) {
  card.msgEl.textContent = text;
  card.msgEl.classList.toggle("error", !!isError);
}

async function loopStart(card, bpm) {
  loopMsg(card, "", false);
  try {
    renderLoop(card, await api("POST", `/api/devices/${encodeURIComponent(card.name)}/start`, { bpm }));
  } catch (e) {
    loopMsg(card, "⚠ 通信エラー", true);
  }
}

async function loopStop(card) {
  loopMsg(card, "戻しています…", false);
  try {
    renderLoop(card, await api("POST", `/api/devices/${encodeURIComponent(card.name)}/stop`));
    loopMsg(card, "", false);
  } catch (e) {
    loopMsg(card, "⚠ 通信エラー", true);
  }
}

/* タップテンポ: 直近 8 間隔の中央値から BPM。最後のタップから 1.5 秒空いたら start。 */
function loopTap(card) {
  const now = performance.now();
  if (card.taps.length && now - card.taps[card.taps.length - 1] > TAP_RESET_MS) card.taps = [];
  card.taps.push(now);
  if (card.taps.length > TAP_KEEP) card.taps.shift();
  if (card.tapTimer) clearTimeout(card.tapTimer);
  if (card.taps.length < 2) {
    card.tapBtn.textContent = "タップ…";
    card.tapTimer = setTimeout(() => { card.tapBtn.textContent = "タップ"; card.taps = []; }, TAP_RESET_MS);
    return;
  }
  const gaps = [];
  for (let i = 1; i < card.taps.length; i++) gaps.push(card.taps[i] - card.taps[i - 1]);
  gaps.sort((a, b) => a - b);
  const mid = gaps.length % 2 ? gaps[(gaps.length - 1) / 2] : (gaps[gaps.length / 2 - 1] + gaps[gaps.length / 2]) / 2;
  const bpm = Math.min(200, Math.max(40, Math.round(60000 / mid)));
  card.tapBtn.textContent = `タップ ${bpm}`;
  card.tapTimer = setTimeout(() => {
    card.taps = [];
    card.tapBtn.textContent = "タップ";
    loopStart(card, bpm);
  }, TAP_SETTLE_MS);
}
```

- [ ] **Step 3: boot のセクションに組み込む**（`const shutters = devices.filter(...)` の直後に `const loops = devices.filter((d) => d.kind === "loop");`、`if (plainSwitches.length) { ... }` ブロックの直後に）

```js
  if (loops.length) {
    panes.ops.appendChild(sectionHeading("🎶 クラブ"));
    for (const dev of loops) panes.ops.appendChild(buildLoopCard(dev));
  }
```

`fetchLightStatesOnce(devices)` / `refreshOnce()` / `startEvents()` が `cards` を走査する作りなら loop は登録されていないので影響なし。`devices.filter((d) => d.kind === "light")` 等の既存フィルタは loop を拾わないことを確認する（`kind === ...` の等値比較のみなら OK）。

- [ ] **Step 4: 手で動作確認**

Run（別ターミナル）: `cat > /tmp/mando_loop_demo.toml <<'EOF'` … 以下の config を書いて `MANDO_CONFIG=/tmp/mando_loop_demo.toml cargo run`:

```toml
[[device]]
name  = "club"
alias = "クラブ（デモ）"
kind  = "loop"
bpms  = [100, 120, 128]
max_minutes = 1
beat = [["sh", "-c", "echo beat $(date +%T.%N) >> /tmp/mando_loop_demo.log"]]
bar  = [["sh", "-c", "echo bar >> /tmp/mando_loop_demo.log"]]
on_stop = [["sh", "-c", "echo stop >> /tmp/mando_loop_demo.log"]]
```

ブラウザで `http://127.0.0.1:8080/` を開き、確認:
1. 「🎶 クラブ」セクションにカードが出て「停止中」。
2. `120` を押す → 「120 BPM で再生中 ・ 残り 1 分」、`120` ボタンに枠。`/tmp/mando_loop_demo.log` に `bar` `beat …` が 0.5 秒ごとに増える。
3. `タップ` を 1 秒間隔で 4 回 → ボタンが「タップ 60」→ 1.5 秒後に「60 BPM で再生中」（40〜200 に丸めるので 60）。
4. `停止` → 「停止中」、log の末尾が `stop`。
5. もう一度 `100` → 1 分放置 → 自動で「停止中」に戻り、log 末尾が `stop`。

Expected: 上記 5 点すべて。`cargo run` を Ctrl-C で止めたとき（再生中）も log 末尾に `stop` が付くこと。

- [ ] **Step 5: lint と commit**

Run: `cargo build 2>&1 | tail -2`（`include_str!` の焼き込みが通ること）

```bash
git add index.html
git commit -m "feat(ui): クラブカード — BPM プリセット・タップテンポ・停止

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: 配備（hogar-iac）と実機確認

**Files:**
- Modify: `~/ghq/github.com/nogu3/hogar-iac/hogar/config/mando/config.toml`（loop デバイスを追加）
- Modify: hogar-iac の mando の pin（despliegue skill の「NAS Docker（hogar）向け」手順が指すファイル）

**Interfaces:**
- Consumes: Task 1〜4 を含む mando の main（Task 4 まで commit 済み・`main` にマージ済み）。

- [ ] **Step 1: mando を `main` に仕上げる**

superpowers:finishing-a-development-branch の流儀どおりローカルで `main` にマージし、`cargo test` が通ることを確認して push（nogu3 private）。

- [ ] **Step 2: hogar-iac の config に loop を追加**（`# ── デスクライト（mat group = desk_room_lights, GroupId 11）──────` の直前）

```toml
# ── クラブ（リビング照明を拍に合わせて回す。mando kind=loop）──────
# 拍の中身はここ（beat / bar / on_stop）。mando は「配列をタイミング付きで exec」するだけ。
# 2026-10-05 実機テスト: 毎拍 100%→8% のパルス + 4 拍ごとに色替え（暗い寒色系）が良かった。
# unicast の順送り（流す演出）は matd が詰まるので groupcast のみ。
[[device]]
name  = "club_living"
alias = "クラブ（リビング）"
kind  = "loop"
bpms  = [100, 120, 128]
max_minutes = 60
bar_beats = 4
beat = [
  ["mat", "group", "level", "--group", "living_lights", "--percent", "100"],
  ["mat", "group", "level", "--group", "living_lights", "--percent", "8", "--transition", "{fade}"],
]
bar = [
  ["mat", "group", "color", "--group", "living_lights", "--name", "magenta"],
  ["mat", "group", "color", "--group", "living_lights", "--name", "blue"],
  ["mat", "group", "color", "--group", "living_lights", "--name", "purple"],
  ["mat", "group", "color", "--group", "living_lights", "--name", "cyan"],
]
on_stop = [
  ["mat", "group", "color-temp", "--group", "living_lights", "--mireds", "370"],
  ["mat", "group", "level", "--group", "living_lights", "--percent", "100"],
]
```

- [ ] **Step 3: pin 更新・build・deploy**

despliegue skill を呼び、「NAS Docker（hogar）向け」手順で mando の pin を Task 4 までを含む commit に更新 → `hogar-apps:local` を build → deploy。deploy 前に新バイナリで config 検査: `ssh nas 'docker run --rm -v /volume1/docker/hogar/config:/config:ro hogar-apps:local mando --check'`（unknown key なし・exit 0）。

- [ ] **Step 4: 実機確認**

1. `ssh nas 'docker logs --tail 20 hogar-mando'` に `mando 起動` と loop に関する警告が無いこと。
2. スマホの mando で「🎶 クラブ」→ `120` → リビングの 7 台が 2026-10-05 のテストと同じく脈打ち、4 拍ごとに色が変わる。20 秒見る。
3. `タップ` を曲に合わせて 4〜8 回 → 表示 BPM で合わせ直る。
4. `停止` → 電球色・100% に戻る。`docker logs` に `loop 停止` が出る。
5. `ssh nas 'cd /volume1/docker/hogar && docker compose restart mando'` を再生中に実行 → 照明が電球色に戻る（shutdown の on_stop）。

Expected: 5 点すべて。2 で揃わない・カクつくなら `max_minutes` ではなく `beat` の 2 発目（フェード）を外して 1 発にし、再確認する（config 編集のみ）。

- [ ] **Step 5: Commit（hogar-iac）**

```bash
cd ~/ghq/github.com/nogu3/hogar-iac
git add hogar/config/mando/config.toml <pin ファイル>
git commit -m "feat(mando): クラブ（リビング）loop デバイスを追加・mando pin 更新

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push
```

---

## Self-Review（記録）

- **Spec coverage**: config（Task 1）、ループ・自動停止・shutdown（Task 2・3）、API（Task 3）、UI（Task 4）、配備（Task 5）。「mando 再起動時に on_stop を走らせない」は looper が起動時に何もしないことで満たす（Task 2 の構造）。「連続 10 拍で warn」は Task 2 実装（テストは失敗継続のみ。warn の回数はログなので検証しない）。
- **Spec との差**: spec は「既存の `Executor::run` を `device.exec_lane()` で」と書いたが、`App.executor` を `Arc` 化すると App 構築 9 箇所に触るため、graph / mesh と同じ**専用 executor** にした（lane はデバイス名で同じ直列化になる。loop に `lane = "echonet"` を書いても他デバイスとは直列化されない — mat 専用の前提なので許容）。spec に 1 行追記済み。
- **Type consistency**: `LoopSpec` / `LoopStatus` / `Looper::{new,status,start,stop,shutdown}` / `fade_ds` / `loop_spec` / `Device::{max_minutes,bar_beats,default_bpm}` の名前と型は Task 2・3 で一致。`run_shutdown` の第 4 引数 `app: Shared` は Task 3 のテストと実装で一致。
- **Review Focus**: 5 件すべてに所有タスクのテスト名を付けた。
