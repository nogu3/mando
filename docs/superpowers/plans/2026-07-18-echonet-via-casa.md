# echonet（シャッター）を casa 経由に移行 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** mando の echonet デバイス（シャッター 5 台）の exec を `enl` 直叩きから `casa` 経由に切り替える。コード変更は正規化関数の casa エンベロープ対応の一点に閉じる。

**Architecture:** casa の get 出力は enl 出力を `{"value": {...}}` でラップする。`normalize_enl_state` を「`value` の中に `properties` があればそれを内側とみなす」よう拡張し、casa 経由・enl 直の両方を受ける（後方互換）。set は casa の echonet アダプタが `enl set` に素通しで写すため、mando からは config 差し替えだけで移行する。jarvis 実機では casa にシャッターを定義し、casa を PATH に置き、mando config をシャッターだけ casa 形に差し替える。

**Tech Stack:** Rust（axum / serde_json）、TOML config、systemd（jarvis）、casa 0.7.1 / enl 1.5。

## Global Constraints

- 対象は echonet シャッター 5 台のみ。matter ライト（living_lights / desk_light）は mat 直叩きのまま変更しない。
- コード変更は `src/normalize.rs` の `normalize_enl_state` 一点のみ。exec.rs / main.rs / config.rs は変更しない（設計原則4）。
- 後方互換必須: 既存の enl 直テストは全通過し続ける。config.example に enl 直の例を残しても正規化が受けること。
- `cargo clippy -- -D warnings` を通す。
- jarvis へのバイナリ配布は `task deploy HOST=jarvis`（ローカル cross ビルド + scp）を使う。手書き手順を増やさない。
- `sudo install` での本番バイナリ上書きは、対象ホスト・バイナリ名・unit 名を復唱してから実行する。
- 実開閉（open/close/stop）は物理的にシャッターが動くため、自動実行しない。実施前にユーザー確認を取る。

---

### Task 1: normalize_enl_state を casa エンベロープ対応にする

**Files:**
- Modify: `src/normalize.rs`（`normalize_enl_state` 関数と `#[cfg(test)]` テスト）

**Interfaces:**
- Consumes: なし（既存 `classify` / `State` をそのまま使う）
- Produces: `pub fn normalize_enl_state(raw: &Value) -> State` — シグネチャ不変。内部で casa エンベロープ（`raw["value"]` が `properties` を持つオブジェクト）を剥がしてから従来処理に入る。

- [ ] **Step 1: casa エンベロープの失敗するテストを追加する**

`src/normalize.rs` の `#[cfg(test)] mod tests` 内、`unknown_on_garbage` テストの後に追加:

```rust
    #[test]
    fn casa_envelope_closed() {
        // casa get の出力は enl 出力を "value" でラップする。
        let raw = json!({
            "device": "shutter1", "protocol": "echonet",
            "timestamp": "2026-07-18T21:00:00+09:00",
            "value": {
                "eoj": "026301", "esv": "GetRes", "ip": "192.168.1.222",
                "properties": [{"edt_hex":"42","epc":"EA","name":"open_close_state",
                                "pdc":1,"value":{"state":"fully_closed"}}]
            }
        });
        assert_eq!(normalize_enl_state(&raw), State::Closed);
    }

    #[test]
    fn casa_envelope_all_five_states() {
        let cases = [
            ("fully_open", State::Open),
            ("fully_closed", State::Closed),
            ("opening", State::Opening),
            ("closing", State::Closing),
            ("stopped_midway", State::Stopped),
        ];
        for (s, want) in cases {
            let raw = json!({
                "device": "shutter1", "protocol": "echonet",
                "value": {"properties":[{"name":"open_close_state","value":{"state":s}}]}
            });
            assert_eq!(normalize_enl_state(&raw), want, "state={s}");
        }
    }

    #[test]
    fn casa_envelope_without_properties_is_unknown() {
        // value はあるが properties を持たない（対象外スキーマ）→ Unknown。
        // enl 直の後方互換を壊さないための番犬。
        let raw = json!({
            "device": "porch_light", "protocol": "echonet",
            "value": {"eoj":"029102","esv":"GetRes",
                      "properties":[{"epc":"80","name":"power","value":{"power":"on"}}]}
        });
        // properties はあるが open_close_state でも既知 state 値でもない → Unknown。
        assert_eq!(normalize_enl_state(&raw), State::Unknown);
    }
```

- [ ] **Step 2: テストが失敗することを確認する**

Run: `cargo test --lib normalize::tests::casa_envelope`
Expected: FAIL — `casa_envelope_closed` / `casa_envelope_all_five_states` が `Unknown`（現状トップレベル `properties` が無いため）で assert 失敗。

- [ ] **Step 3: normalize_enl_state にエンベロープ剥がしを実装する**

`src/normalize.rs` の `normalize_enl_state` 冒頭を次のように変更する。現在の実装:

```rust
pub fn normalize_enl_state(raw: &Value) -> State {
    let Some(props) = raw.get("properties").and_then(Value::as_array) else {
        return State::Unknown;
    };
```

を、これに置き換える:

```rust
pub fn normalize_enl_state(raw: &Value) -> State {
    // casa get は enl 出力を {"value": {...}} でラップする。value の中に
    // properties があればそれを内側とみなし、無ければ raw 自身を使う。
    // これで casa 経由・enl 直の両方を受ける（設計原則4の一点変更）。
    let inner = raw
        .get("value")
        .filter(|v| v.get("properties").is_some())
        .unwrap_or(raw);
    let Some(props) = inner.get("properties").and_then(Value::as_array) else {
        return State::Unknown;
    };
```

以降の関数本体（`props.iter().find(...)` 〜）は一切変更しない。

- [ ] **Step 4: テストが通ることを確認する**

Run: `cargo test --lib normalize`
Expected: PASS — 新規 casa エンベロープ 3 テスト + 既存 enl 直テスト（`real_enl_format` / `all_five_states` / `unknown_on_garbage` 等）が全通過。

- [ ] **Step 5: 全テストと clippy を通す**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: PASS / warnings なし。

- [ ] **Step 6: コミット**

```bash
git add src/normalize.rs
git commit -m "feat: normalize_enl_state を casa エンベロープ対応に（enl 直と両対応）

casa get は enl 出力を {\"value\": {...}} でラップする。value 内に
properties があれば内側とみなす一点変更で casa 経由・enl 直の両方を受ける。
既存の enl 直テストは全通過（後方互換）。設計原則4の唯一のコード変更点。

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KdELFYS55V645tc9UsweTo"
```

---

### Task 2: config.example.toml のシャッター例を casa リードに書き換える

**Files:**
- Modify: `config.example.toml`（先頭の `[[device]] name = "shutter"` ブロックと直後の casa コメント例）

**Interfaces:**
- Consumes: なし（ドキュメントのみ）
- Produces: なし（挙動に影響しない）

- [ ] **Step 1: シャッター例を casa 形に書き換える**

`config.example.toml` の `[[device]] name = "shutter"` ブロックの `get_state` /
`open` / `close` / `stop` 行と、その下の「将来 casa に差し替える例」コメントブロックを
探す。現在:

```toml
get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
open      = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "open"]
close     = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "close"]
# stop は任意。指定すると UI に「止める」ボタンが出る（途中停止）。未指定なら出ない。
stop      = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "stop"]

# 将来 casa に差し替える例（本体コードは変更しない）:
#   get_state = ["casa", "get", "shutter", "open_close_state"]
#   open      = ["casa", "set", "shutter", "open_close_operation", "open"]
#   close     = ["casa", "set", "shutter", "open_close_operation", "close"]
```

を、casa をリードにし enl 直を代替として併記する形に置き換える:

```toml
# casa 経由（推奨。名前解決は casa の devices.toml が持つ）。
# casa get は enl 出力を {"value": {...}} でラップするが、mando の正規化が
# 両対応なので下の enl 直の形にそのまま差し戻すこともできる。
get_state = ["casa", "get", "shutter", "open_close_state"]
open      = ["casa", "set", "shutter", "open_close_operation", "open"]
close     = ["casa", "set", "shutter", "open_close_operation", "close"]
# stop は任意。指定すると UI に「止める」ボタンが出る（途中停止）。未指定なら出ない。
stop      = ["casa", "set", "shutter", "open_close_operation", "stop"]

# enl を直接叩く形（casa を挟まない場合。正規化はこちらも受ける）:
#   get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
#   open      = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "open"]
#   close     = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "close"]
#   stop      = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "stop"]
```

`alias` 行やその上の EPC 確認コメント（`enl describe <IP> 026301`）はそのまま残す。
mat ライト・グラフ・health のサンプルは変更しない。

- [ ] **Step 2: config.example.toml がパースできることを確認する**

Run: `cargo run -- --help 2>/dev/null; python3 -c "import tomllib; tomllib.load(open('config.example.toml','rb')); print('toml ok')"`
Expected: `toml ok`（TOML として妥当）。

- [ ] **Step 3: コミット**

```bash
git add config.example.toml
git commit -m "docs: config.example のシャッター例を casa リードに（enl 直は代替として併記）

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KdELFYS55V645tc9UsweTo"
```

---

### Task 3: jarvis の casa にシャッター 5 台を定義する

**Files:**
- Modify（jarvis 実機・リポジトリ外）: `~/.config/casa/devices.toml`

**Interfaces:**
- Consumes: なし
- Produces: casa デバイス名 `shutter1`..`shutter5`（Task 5 の mando config が参照する）

- [ ] **Step 1: 現在の devices.toml をバックアップする**

Run:
```bash
ssh jarvis 'cp ~/.config/casa/devices.toml ~/.config/casa/devices.toml.bak.$(hostname).echonet-casa'
```
Expected: エラーなし（`.bak.<host>.echonet-casa` が作られる）。

- [ ] **Step 2: シャッター 5 台を devices.toml の末尾に追記する**

Run（heredoc で追記。eoj は 0x 付き文字列で casa の既存流儀に揃える）:
```bash
ssh jarvis "cat >> ~/.config/casa/devices.toml" <<'EOF'

# --- 電動シャッター (192.168.1.222 / 0x0263)。mando が casa 経由で開閉する ---

[devices.shutter1]   # リビング
protocol = "echonet"
ip = "192.168.1.222"
eoj = "0x026301"

[devices.shutter2]   # 南側FIX
protocol = "echonet"
ip = "192.168.1.222"
eoj = "0x026302"

[devices.shutter3]   # 西側FIX
protocol = "echonet"
ip = "192.168.1.222"
eoj = "0x026303"

[devices.shutter4]   # 腰窓南
protocol = "echonet"
ip = "192.168.1.222"
eoj = "0x026304"

[devices.shutter5]   # 腰窓北
protocol = "echonet"
ip = "192.168.1.222"
eoj = "0x026305"
EOF
```
Expected: エラーなし。

- [ ] **Step 3: casa list にシャッター 5 台が出ることを確認する**

Run:
```bash
ssh jarvis 'export PATH=$HOME/.cargo/bin:$PATH; casa list | tr "," "\n" | grep -c shutter'
```
Expected: `5`（shutter1..5 が名前として出る。各エントリに name が 1 回ずつ = 5 マッチ以上。0 でないこと）。

- [ ] **Step 4: casa get でシャッターの状態が読めることを確認する（非破壊）**

Run:
```bash
ssh jarvis 'export PATH=$HOME/.cargo/bin:$PATH; casa get shutter1 open_close_state'
```
Expected: `{"device":"shutter1","protocol":"echonet","timestamp":"...","value":{"eoj":"026301",...,"properties":[{...,"name":"open_close_state","value":{"state":"fully_open|fully_closed|..."}}]}}` のような casa エンベロープ JSON。`value.properties` に open_close_state が入っていること。

> このタスクはコミット不要（jarvis 実機のリポジトリ外設定）。Step 4 が通ったことを記録する。

---

### Task 4: casa を /usr/local/bin へ install する

**Files:**
- 配置（jarvis 実機）: `/usr/local/bin/casa`（既存 `~/.local/bin/casa` 0.7.1 をコピー）

**Interfaces:**
- Consumes: なし
- Produces: mando サービスの PATH（`/usr/local/bin`）から bare `casa` が解決可能になる。

- [ ] **Step 1: install 対象を復唱してから /usr/local/bin へ配置する**

復唱: **ホスト = jarvis、バイナリ = casa（~/.local/bin/casa 0.7.1）→ /usr/local/bin/casa。unit の再起動は伴わない（バイナリ配置のみ）。**

Run:
```bash
ssh jarvis 'sudo install -Dm755 ~/.local/bin/casa /usr/local/bin/casa'
```
Expected: エラーなし。

- [ ] **Step 2: /usr/local/bin/casa がバージョンを返すことを確認する**

Run:
```bash
ssh jarvis '/usr/local/bin/casa --version'
```
Expected: `casa 0.7.1`。

- [ ] **Step 3: mando サービスの PATH で bare casa が解決できることを確認する**

Run（mando unit の PATH を再現して確認）:
```bash
ssh jarvis 'env -i PATH=/home/jarvis/.cargo/bin:/usr/local/bin:/usr/bin:/bin sh -c "command -v casa && casa get shutter1 open_close_state >/dev/null 2>&1 && echo bare-casa-ok"'
```
Expected: `/usr/local/bin/casa` と `bare-casa-ok`（bare `casa` が解決し、get も成功する）。

> このタスクはコミット不要（jarvis 実機のバイナリ配置）。Step 2・3 が通ったことを記録する。

---

### Task 5: mando 新バイナリをデプロイする

**Files:**
- 配布（jarvis 実機）: `/usr/local/bin/mando`（Task 1 の normalize 変更を含む）

**Interfaces:**
- Consumes: Task 1 でコミットされた normalize 変更（main ブランチに入っていること）
- Produces: casa エンベロープ対応の mando が jarvis で稼働。config はまだ enl のままでも両対応で稼働継続。

- [ ] **Step 1: Task 1・2 のコミットが main に入っていることを確認する**

Run:
```bash
cd ~/ghq/github.com/nogu3/mando && git log --oneline -3
```
Expected: Task 1（normalize casa 対応）と Task 2（config.example）のコミットが見える。

- [ ] **Step 2: task deploy でクロスビルド → 転送 → 再起動する**

復唱: **ホスト = jarvis、バイナリ = mando → /usr/local/bin/mando、unit = mando を restart。**

Run:
```bash
cd ~/ghq/github.com/nogu3/mando && task deploy HOST=jarvis
```
Expected: `cross build` 成功 → scp → `sudo install` → `systemctl restart mando` → `systemctl status` が `active (running)`。

> **cross（docker）がこの環境で使えない場合のフォールバック**: [[jarvis-mando-deploy-path]] の手順（ローカルから `git push ssh://jarvis/.../mando main:refs/heads/deploy-incoming` → jarvis 側で ff → `cargo build --release` → `sudo install -Dm755 target/release/mando /usr/local/bin/mando` → `sudo systemctl restart mando`）。HEAD がローカル main と一致することを必ず確認する。

- [ ] **Step 3: mando が起動し devices を返すことを確認する（config はまだ enl）**

Run:
```bash
ssh jarvis 'curl -s http://localhost:8080/api/devices/shutter1/state'
```
Expected: `{"state":"open|closed|...","raw":{...}}` — enl 直の config でも従来どおり正規化 state が返る（後方互換の確認）。

> このタスクはコミット不要（バイナリ配布）。Step 3 が通ったことを記録する。

---

### Task 6: mando config のシャッター 5 台を casa 形に差し替える

**Files:**
- Modify（jarvis 実機・リポジトリ外）: `/etc/mando/config.toml`（shutter1..5 の 4 行ずつ）

**Interfaces:**
- Consumes: Task 3（casa の shutter1..5）、Task 4（bare casa）、Task 5（casa 対応 mando）
- Produces: mando の echonet exec が casa 経由になる。

- [ ] **Step 1: 現在の mando config をバックアップする**

Run:
```bash
ssh jarvis 'cp /etc/mando/config.toml /etc/mando/config.toml.bak.echonet-casa'
```
Expected: エラーなし（config.toml は jarvis ユーザー所有なので sudo 不要）。

- [ ] **Step 2: shutter1..5 の enl 配列を casa 配列に置換する**

Run（sed で 5 台分を一括置換。IP+EPC の enl 形を device 名の casa 形へ）:
```bash
ssh jarvis 'sed -i -E \
  -e "s#\[\"enl\", \"get\", \"192.168.1.222\", \"02630([1-5])\", \"open_close_state\"\]#[\"casa\", \"get\", \"shutter\1\", \"open_close_state\"]#" \
  -e "s#\[\"enl\", \"set\", \"192.168.1.222\", \"02630([1-5])\", \"open_close_operation\", \"(open|close|stop)\"\]#[\"casa\", \"set\", \"shutter\1\", \"open_close_operation\", \"\2\"]#" \
  /etc/mando/config.toml'
```
Expected: エラーなし。

- [ ] **Step 3: 置換結果を確認する（enl の 192.168.1.222 行が残っていないこと）**

Run:
```bash
ssh jarvis 'grep -c "192.168.1.222" /etc/mando/config.toml; echo "--- casa shutter 行 ---"; grep "casa.*shutter" /etc/mando/config.toml'
```
Expected: 1 行目 `0`（enl の実 IP 行が全て casa 形に置換された）。`casa`, `get/set`, `shutter1..5` の行が 20 行（5 台 × get/open/close/stop）出る。mat ライトの行は変更されていない。

- [ ] **Step 4: mando を再起動して config を反映する**

Run:
```bash
ssh jarvis 'sudo systemctl restart mando && systemctl status mando --no-pager --lines=0'
```
Expected: `active (running)`。

- [ ] **Step 5: casa 経由で state が取れることを確認する（非破壊・全 5 台）**

Run:
```bash
ssh jarvis 'for n in 1 2 3 4 5; do echo -n "shutter$n: "; curl -s http://localhost:8080/api/devices/shutter$n/state | head -c 200; echo; done'
```
Expected: 各シャッターで `{"state":"open|closed|opening|closing|stopped|unknown","raw":{...}}`。`state` が `unknown` でない（casa エンベロープが正しく剥がれて正規化されている）こと。`raw` に casa エンベロープ（`device`/`protocol`/`value`）が入っていること。

> このタスクはコミット不要（jarvis 実機設定）。Step 5 が通ったことを記録する。

---

### Task 7: 実開閉の手動検証（ユーザー確認後）

**Files:** なし（実機での動作確認のみ）

**Interfaces:**
- Consumes: Task 6 完了（config が casa 形、state 取得 OK）
- Produces: casa 経由の set→state 再取得が実際に開閉を反映することの確証。

- [ ] **Step 1: ユーザーに実開閉テストの可否を確認する**

シャッターが物理的に動くため、実施前にユーザーへ「1 台（shutter1 = リビング）で open→close を試してよいか」を確認する。**承認が得られるまで Step 2 以降を実行しない。**

- [ ] **Step 2: 1 台で open → state 再取得を検証する（承認後）**

Run:
```bash
ssh jarvis 'curl -s -X POST http://localhost:8080/api/devices/shutter1/open'
```
Expected: mando が casa 経由で `enl set` を実行し、set 後に state を再取得した結果（`{"state":"open|opening|...","raw":{...}}`）を返す。楽観表示でなく実測 state が入っていること。シャッターが実際に開き始めること。

- [ ] **Step 3: 1 台で close → state 再取得を検証する（承認後）**

Run:
```bash
ssh jarvis 'curl -s -X POST http://localhost:8080/api/devices/shutter1/close'
```
Expected: `{"state":"closed|closing|...","raw":{...}}`。シャッターが実際に閉じ始めること。

- [ ] **Step 4: 検証結果をユーザーに報告する**

state の遷移が実動作と一致したことを報告する。異常（`unknown` / timeout / SNA）があれば終了コードマッピング（3=timeout/4=SNA/5=network）と casa の error 出力を添えて報告する。

> このタスクはコミット不要（動作検証）。

---

## デプロイ順序まとめ

1. Task 1・2（ローカル: コード + example、コミット）
2. Task 3・4（jarvis casa: shutter 定義 + /usr/local/bin install、非破壊 get で疎通）
3. Task 5（jarvis mando: 新バイナリ、config はまだ enl でも両対応で稼働）
4. Task 6（jarvis mando config: casa 形に差し替え、非破壊 state で確認）
5. Task 7（ユーザー承認後に実開閉）

## 関連

- スペック: `docs/superpowers/specs/2026-07-18-echonet-via-casa-design.md`
- [[jarvis-mando-deploy-path]] — デプロイのフォールバック手順
- [[casad-holds-3610]] — 3610 共存の前提
