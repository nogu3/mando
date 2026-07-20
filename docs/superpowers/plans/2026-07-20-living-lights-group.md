# リビング照明グループカード化(members 方式) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** members を持つ light デバイスを「グループカード+個別に操作 ▾ 展開」として描画し、リビング照明 6 台を 1 枚のカードにまとめる。

**Architecture:** 既存の `living_lights`(mat wire group、multicast 一括操作)デバイスに config で `members` を持たせる。バックエンドは検証と `/api/devices` への公開のみ、新規エンドポイントなし。UI はシャッターグループと同じ「カード+expander」流儀で、展開内にメンバーのフル機能 light タイル(既存 `buildLightTile` 再利用)を並べる。

**Tech Stack:** Rust (axum / serde / toml)、vanilla JS(index.html 焼き込み)、Ansible(jarvis-iac)

**Spec:** `docs/superpowers/specs/2026-07-19-living-lights-group-design.md`

## Global Constraints

- 新規 API エンドポイントを作らない。`/api/devices` の各要素に `members` を足すだけ(空なら `skip_serializing_if` でフィールドごと省略)
- members は親・メンバーとも `kind = "light"` 限定。入れ子禁止・複数親への所属禁止
- 一括操作は既存の living_lights のコマンド(wire group multicast)のまま。members を直列 exec する一括操作は作らない
- グループカードの状態表示は現状どおり代表ノード読み(全メンバーポーリングはしない)
- メンバーの `/api/devices/{name}/...` 各エンドポイントは従来どおり全部生きる
- コミットメッセージ末尾に必ず付ける:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01EBYi6KhS32NKio3GyWykbH`
- `cargo clippy -- -D warnings` が通ること

---

### Task 1: config — `Device.members` + 検証 + config.example

**Files:**
- Modify: `src/config.rs`(Device struct ~L144-185、ConfigError ~L229-296、validate ~L340-507、tests ~L522-)
- Modify: `config.example.toml`

**Interfaces:**
- Produces: `Device.members: Vec<String>`(serde default 空)。Task 2 が `d.members.clone()` で読む。
- Produces: `ConfigError::{UnknownLightMember, NonLightMember, NestedLightMembers, DuplicateLightMember}` バリアント。

- [ ] **Step 1: 失敗するテストを書く**

`src/config.rs` の `mod tests` 末尾に追加(既存の `write_tmp` ヘルパを使う)。light デバイスは `get_state`/`on`/`off` が必須なので短い共通文字列で組み立てる:

```rust
    /// members テスト用の light デバイス定義を生成する。
    fn light_toml(name: &str, extra: &str) -> String {
        format!(
            r##"
            [[device]]
            name = "{name}"
            kind = "light"
            get_state = ["mat","read"]
            on  = ["mat","on"]
            off = ["mat","off"]
            {extra}
            "##
        )
    }

    #[test]
    fn light_members_parse() {
        let body = format!(
            "{}{}{}",
            light_toml("parent", r#"members = ["kid1", "kid2"]"#),
            light_toml("kid1", ""),
            light_toml("kid2", ""),
        );
        let p = write_tmp("members_ok", &body);
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.find("parent").unwrap().members, vec!["kid1", "kid2"]);
        assert!(cfg.find("kid1").unwrap().members.is_empty());
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_unknown() {
        let body = light_toml("parent", r#"members = ["ghost"]"#);
        let p = write_tmp("members_unknown", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::UnknownLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_non_light() {
        let body = format!(
            "{}{}",
            light_toml("parent", r#"members = ["sw"]"#),
            r##"
            [[device]]
            name = "sw"
            kind = "switch"
            get_state = ["casa","get"]
            on  = ["casa","on"]
            off = ["casa","off"]
            "##
        );
        let p = write_tmp("members_nonlight", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NonLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_nested() {
        let body = format!(
            "{}{}{}",
            light_toml("grandparent", r#"members = ["parent"]"#),
            light_toml("parent", r#"members = ["kid"]"#),
            light_toml("kid", ""),
        );
        let p = write_tmp("members_nested", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::NestedLightMembers { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_reject_duplicate_membership() {
        let body = format!(
            "{}{}{}",
            light_toml("p1", r#"members = ["kid"]"#),
            light_toml("p2", r#"members = ["kid"]"#),
            light_toml("kid", ""),
        );
        let p = write_tmp("members_dup", &body);
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::DuplicateLightMember { .. })
        ));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn light_members_forbidden_on_shutter() {
        let p = write_tmp(
            "members_shutter",
            r##"
            [[device]]
            name = "s1"
            members = ["s2"]
            get_state = ["enl","get","x","026301","open_close_state"]
            open = ["enl","set","x","026301","open_close_operation","open"]
            close = ["enl","set","x","026301","open_close_operation","close"]
            [[device]]
            name = "s2"
            get_state = ["enl","get","x","026302","open_close_state"]
            open = ["enl","set","x","026302","open_close_operation","open"]
            close = ["enl","set","x","026302","open_close_operation","close"]
            "##,
        );
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::ForbiddenField { field: "members", .. })
        ));
        std::fs::remove_file(p).ok();
    }
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test light_members`
Expected: コンパイルエラー(`members` フィールドも `UnknownLightMember` も未定義)

- [ ] **Step 3: 実装**

`src/config.rs` の `Device` struct(`face` フィールドの直後、L184 付近)にフィールド追加:

```rust
    /// このデバイスをグループカードとして描画するときのメンバー device 名(light 専用・任意)。
    /// 一括操作はこのデバイス自身のコマンド(wire group 等)が担い、members は
    /// UI の「個別に操作」展開に使うだけ — mando が members を直列 exec することはない。
    #[serde(default)]
    pub members: Vec<String>,
```

`ConfigError` enum(`EmptyHealthCommand` の直前)にバリアント追加:

```rust
    UnknownLightMember { device: String, member: String },
    NonLightMember { device: String, member: String },
    NestedLightMembers { device: String, member: String },
    DuplicateLightMember { member: String },
```

`Display` impl(`EmptyHealthCommand` の arm の直前)に追加:

```rust
            ConfigError::UnknownLightMember { device, member } => {
                write!(f, "device {device}: members が未知の device を参照: {member}")
            }
            ConfigError::NonLightMember { device, member } => {
                write!(f, "device {device}: members に light 以外は入れられない: {member}")
            }
            ConfigError::NestedLightMembers { device, member } => {
                write!(f, "device {device}: member {member} は自身が members を持つため入れ子にできない")
            }
            ConfigError::DuplicateLightMember { member } => {
                write!(f, "device {member}: 複数の light グループに所属できない")
            }
```

`validate()` のデバイス個別ループの直後・`[[group]]` 検証ブロックの直前(L448 手前)に追加。デバイス相互参照なのでループの外:

```rust
        // light の members(グループカード)検証。親・メンバーとも light 限定、
        // 入れ子と複数親への所属は禁止(UI がタイルをデバイスごとに 1 つしか持てないため)。
        let mut seen_lm = std::collections::HashSet::new();
        for d in &self.devices {
            if d.members.is_empty() {
                continue;
            }
            if d.kind != Kind::Light {
                return Err(ConfigError::ForbiddenField {
                    device: d.name.clone(),
                    field: "members",
                });
            }
            for m in &d.members {
                let Some(md) = self.find(m) else {
                    return Err(ConfigError::UnknownLightMember {
                        device: d.name.clone(),
                        member: m.clone(),
                    });
                };
                if md.kind != Kind::Light {
                    return Err(ConfigError::NonLightMember {
                        device: d.name.clone(),
                        member: m.clone(),
                    });
                }
                if !md.members.is_empty() {
                    return Err(ConfigError::NestedLightMembers {
                        device: d.name.clone(),
                        member: m.clone(),
                    });
                }
                if !seen_lm.insert(m) {
                    return Err(ConfigError::DuplicateLightMember { member: m.clone() });
                }
            }
        }
```

注: 自己参照(`members = ["自分"]`)は「自分が members を持つ」ので `NestedLightMembers` で弾ける — 専用チェック不要。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test light_members`
Expected: 6 テスト PASS

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全テスト PASS、clippy 警告なし

- [ ] **Step 5: config.example.toml に例を追記**

`config.example.toml` の mat wire group デバイス(`living_lights` 相当)のセクションに members の例とコメントを追記。既存の living_lights 例の `[[device]]` ブロック内(`brightness = ...` 行の後)に:

```toml
# members: このカードの「個別に操作 ▾」展開に入れるメンバー(light 専用・任意)。
# 一括操作は上の group コマンドのまま。members は UI の入れ子表示にだけ使われ、
# mando が members を順に exec することはない。メンバーは自分も [[device]] として
# 定義されている必要がある(トップの照明一覧からは消え、展開内に入る)。
members = ["living_south_light", "tv_back_light"]
```

さらに members が参照する 2 台が例として存在するよう、単体ノード light の例(desk_light 相当)と同じ形式で `living_south_light` / `tv_back_light` の `[[device]]` 例を追加する(既存の単体ノード例をコピーして name / --node 引数を差し替え)。

- [ ] **Step 6: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat(config): light デバイスに members(グループカード用)を追加"
```

---

### Task 2: API — `/api/devices` に members を公開

**Files:**
- Modify: `src/main.rs`(DeviceInfo ~L130-145、list_devices ~L147-172、tests ~L767-)

**Interfaces:**
- Consumes: `Device.members: Vec<String>`(Task 1)
- Produces: `/api/devices` の各要素の `members: string[]` フィールド(空なら省略)。Task 3 の UI が `d.members || []` で読む。

- [ ] **Step 1: 失敗するテストを書く**

`src/main.rs` の `mod tests` に追加。既存の `call_on(config, method, path)` ヘルパ(カスタム config で 1 リクエスト投げる)を使う:

```rust
    #[tokio::test]
    async fn devices_list_has_members_only_when_present() {
        let cfg = r##"
            [[device]]
            name = "parent"
            kind = "light"
            members = ["kid"]
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
            [[device]]
            name = "kid"
            kind = "light"
            get_state = ["sh", "-c", "printf '{\"value\":true}'"]
            on  = ["sh", "-c", "printf '{}'"]
            off = ["sh", "-c", "printf '{}'"]
        "##;
        let (st, v) = call_on(cfg, "GET", "/api/devices").await;
        assert_eq!(st, StatusCode::OK);
        let arr = v.as_array().unwrap();
        let parent = arr.iter().find(|d| d["name"] == "parent").unwrap();
        assert_eq!(parent["members"], serde_json::json!(["kid"]));
        // 空の members はフィールドごと省略される。
        let kid = arr.iter().find(|d| d["name"] == "kid").unwrap();
        assert!(kid.get("members").is_none());
    }
```

(`call_on` の実シグネチャが `(cfg: &str, ...)` でなく別形なら既存テストの呼び方に合わせること。)

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test devices_list_has_members`
Expected: FAIL(`parent["members"]` が null)

- [ ] **Step 3: 実装**

`DeviceInfo`(L144 `face` フィールドの後)に追加:

```rust
    /// members を持つ light(グループカード)のメンバー device 名。空なら省略。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    members: Vec<String>,
```

`list_devices` の `map` 内(`face: d.face,` の後)に追加:

```rust
            members: d.members.clone(),
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全テスト PASS、clippy 警告なし

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(api): /api/devices に members を公開(空なら省略)"
```

---

### Task 3: UI — 照明グループカード(index.html)

**Files:**
- Modify: `index.html`(CSS ~L115 付近、`buildLightTile` 直後 ~L569、`boot()` の照明セクション ~L1286-1300)

**Interfaces:**
- Consumes: `/api/devices` 各要素の `members: string[]`(無ければ undefined。Task 2)
- Produces: なし(最終消費者)

- [ ] **Step 1: buildLightGroupCard を追加**

`index.html` の `buildLightTile`(~L569 `}` の後)の直後に追加:

```js
/* ── 照明グループカード(members を持つ light)──────────────
   カード本体は既存の light タイルそのまま(一括操作 = wire group への multicast)。
   「個別に操作 ▾」でメンバーのフル機能タイルを展開する(シャッターと同じ流儀)。 */
function buildLightGroupCard(dev, byName) {
  const el = document.createElement("div");
  el.className = "group lgroup";
  const head = document.createElement("div");
  head.className = "tiles head";
  head.appendChild(buildLightTile(dev));
  el.appendChild(head);
  const exp = document.createElement("button");
  exp.className = "expander";
  exp.type = "button";
  exp.textContent = "個別に操作 ▾";
  el.appendChild(exp);
  const membersEl = document.createElement("div");
  membersEl.className = "tiles members";
  membersEl.hidden = true;
  for (const m of dev.members) {
    const md = byName.get(m);
    if (md) membersEl.appendChild(buildLightTile(md));
  }
  el.appendChild(membersEl);
  exp.addEventListener("click", () => {
    membersEl.hidden = !membersEl.hidden;
    exp.textContent = membersEl.hidden ? "個別に操作 ▾" : "個別に操作 ▴";
  });
  return el;
}
```

- [ ] **Step 2: boot() の照明セクションを差し替え**

現在の L1287-1300 相当:

```js
  const lights = devices.filter((d) => d.kind === "light");
  const switchLights = devices.filter((d) => d.kind === "switch" && d.face === "light");
  const plainSwitches = devices.filter((d) => d.kind === "switch" && d.face !== "light");
  const shutters = devices.filter((d) => d.kind === "shutter");
  const grouped = new Set(grps.flatMap((g) => g.members));

  if (lights.length || switchLights.length) {
    app.appendChild(sectionHeading("💡 照明"));
    const tiles = document.createElement("div");
    tiles.className = "tiles";
    for (const dev of lights) tiles.appendChild(buildLightTile(dev));
    for (const dev of switchLights) tiles.appendChild(buildSwitchTile(dev));
    app.appendChild(tiles);
  }
```

を次に差し替える(`plainSwitches` / `shutters` / `grouped` の行は変更なし):

```js
  const lights = devices.filter((d) => d.kind === "light");
  const switchLights = devices.filter((d) => d.kind === "switch" && d.face === "light");
  const plainSwitches = devices.filter((d) => d.kind === "switch" && d.face !== "light");
  const shutters = devices.filter((d) => d.kind === "shutter");
  const grouped = new Set(grps.flatMap((g) => g.members));
  // members に挙がった light はトップの一覧から消え、グループカードの展開内に入る。
  const lightMemberSet = new Set(lights.flatMap((d) => d.members || []));
  const lightGroups = lights.filter((d) => (d.members || []).length);
  const soloLights = lights.filter(
    (d) => !(d.members || []).length && !lightMemberSet.has(d.name)
  );

  if (lights.length || switchLights.length) {
    app.appendChild(sectionHeading("💡 照明"));
    for (const g of lightGroups) app.appendChild(buildLightGroupCard(g, byName));
    if (soloLights.length || switchLights.length) {
      const tiles = document.createElement("div");
      tiles.className = "tiles";
      for (const dev of soloLights) tiles.appendChild(buildLightTile(dev));
      for (const dev of switchLights) tiles.appendChild(buildSwitchTile(dev));
      app.appendChild(tiles);
    }
  }
```

- [ ] **Step 3: CSS を追加**

`.tiles` 定義(~L115)の直後に追加:

```css
  /* 照明グループカード: 先頭タイルは全幅、展開内は通常の 2 列グリッド */
  .lgroup > .tiles { margin: 0; }
  .lgroup > .tiles.head { grid-template-columns: 1fr; }
  .lgroup > .tiles.members { margin-top: 4px; }
```

- [ ] **Step 4: ローカルで目視確認(ヘッドレス Chromium スクリーンショット)**

sh 偽装 config でサーバを起動:

```bash
cat > /tmp/claude-1000/-home-noguk-ghq-github-com-nogu3-mando/90ed381d-3630-4fb2-89b0-1c3840ede137/scratchpad/ui-test.toml <<'EOF'
bind = "127.0.0.1:18080"
[[device]]
name = "living_lights"
alias = "リビング照明"
kind = "light"
members = ["l1", "l2", "l3"]
get_state = ["sh", "-c", "printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]
[[device]]
name = "l1"
alias = "リビング南"
kind = "light"
get_state = ["sh", "-c", "printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]
[[device]]
name = "l2"
alias = "テレビ裏"
kind = "light"
get_state = ["sh", "-c", "printf '{\"value\":false}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]
[[device]]
name = "l3"
alias = "あかり"
kind = "light"
get_state = ["sh", "-c", "printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]
[[device]]
name = "desk_light"
alias = "デスクライト"
kind = "light"
get_state = ["sh", "-c", "printf '{\"value\":true}'"]
on  = ["sh", "-c", "printf '{}'"]
off = ["sh", "-c", "printf '{}'"]
EOF
MANDO_CONFIG=/tmp/claude-1000/-home-noguk-ghq-github-com-nogu3-mando/90ed381d-3630-4fb2-89b0-1c3840ede137/scratchpad/ui-test.toml cargo run &
```

(config パスの渡し方は `rg -n "MANDO_CONFIG|config.toml" src/main.rs` で確認し、env でなく引数ならそれに合わせる。)

WSL2 のヘッドレス Chromium(メモリ headless-chromium-on-wsl2 の playwright-core 構成)で、折りたたみ時と「個別に操作」クリック後の 2 枚(390x844 のモバイルビューポート)を撮り、以下を目視確認する:

- 「💡 照明」セクション先頭に「リビング照明」カードが全幅で出る
- カード内に「個別に操作 ▾」があり、クリックで リビング南 / テレビ裏 / あかり のフル機能タイル(💡ボタン+ステータス)が 2 列で展開される
- デスクライトはトップの一覧に残り、メンバー 3 台はトップに重複表示されない

確認後、起動した cargo run を kill する。

- [ ] **Step 5: テスト・lint 通過を確認して Commit**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 全テスト PASS(index.html は include_str! なので rebuild される)

```bash
git add index.html
git commit -m "feat(ui): members を持つ light をグループカード+個別展開で描画"
```

---

### Task 4: デプロイと実機確認(jarvis)

**Files:**
- Modify: `~/ghq/github.com/nogu3/jarvis-iac/roles/mando/files/config.toml`(living_lights の `[[device]]` ブロック)

**Interfaces:**
- Consumes: Task 1-3 を含む mando バイナリと members config

- [ ] **Step 1: mando バイナリを jarvis へデプロイ**

despliegue skill の手順に従う(標準は `task deploy HOST=jarvis`)。

- [ ] **Step 2: jarvis-iac に members を追記**

`roles/mando/files/config.toml` の living_lights の `[[device]]` ブロック(`brightness = ...` 行の後、最初の `[[device.preset]]` の前)に追加:

```toml
# 「個別に操作 ▾」展開に入れる wire group の実メンバー 6 台(ユーザー確認済み)。
members = [
  "living_south_light",
  "living_west_light",
  "living_north_light",
  "tv_back_light",
  "dropped_ceiling_light",
  "akari",
]
```

注意: jarvis-iac の作業開始時はまず `ansible-playbook site.yml --check --diff` で drift を確認する(jarvis skill のルール)。config.toml に他セッションの未コミット変更が残っている場合は巻き込まない(コミットは mando config の members 追記分のみか、ユーザーに確認)。

- [ ] **Step 3: dry-run で差分確認 → 本適用**

```bash
cd ~/ghq/github.com/nogu3/jarvis-iac
ansible-playbook site.yml --check --diff   # 差分が members 追記だけであることを確認
ansible-playbook site.yml                  # mando が handler で再起動される
```

- [ ] **Step 4: 実機で確認**

```bash
ssh jarvis 'systemctl --user is-active mando'
ssh jarvis 'curl -fsS http://localhost:8080/api/devices' | python3 -m json.tool | grep -A8 '"living_lights"'
```

Expected: `active`、living_lights に `"members": [...6 台...]`。
さらに `http://192.168.1.190:8080/` をヘッドレス Chromium で開き、実機でもグループカード+展開が出ることをスクリーンショットで確認する(点灯操作はしない)。

- [ ] **Step 5: Commit(jarvis-iac)**

```bash
cd ~/ghq/github.com/nogu3/jarvis-iac
git add roles/mando/files/config.toml
git commit -m "mando: living_lights に members(グループカード)を追加"
```

(他セッションの未コミット変更が同ファイルに残っている場合はこの commit をスキップし、ユーザーへ報告する。)
