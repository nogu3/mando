# CLAUDE.md

`mando` — スマートホーム操作の **Web フロント**。スマホから家電を操作するための常駐 HTTP サービス。プロトコルは喋らず、`casa`（ブートストラップ期は `enl`）をサブプロセスで呼ぶ。

> 名前: **`mando`**（西: リモコン）。`casa`（家）に対する「家のリモコン」。
> 設定の扱い: **実 config（実 IP・EPC＝設置環境のトポロジ）はリポジトリに含めない。** 同梱するのはサンプル（`config.example.toml`）のみ。
> ファミリ内での位置: `enl` / `casa` が one-shot CLI なのに対し、**`mando` はこのファミリ初の常駐サービス**。この非対称は意図的（後述）。

---

## 位置づけ（3 層）

| 層 | 例 | 形態 | 役割 |
|---|---|---|---|
| プロトコル | `enl`, `sbl`(仮), `mat`(仮) | one-shot CLI | プロトコルを喋る |
| 横断 | `casa` | one-shot CLI | 名前解決・統一 UX。上記を subprocess で呼ぶ |
| 提供 | **`mando`** | 常駐 HTTP サービス | UI を配り、`casa` を subprocess で呼ぶ |

`mando` は最上層。**下に何があるかは config で決まり、本体コードは知らない。**

---

## 目的

- スマホ（特に技術者でない利用者）から、シャッター等を**確実に**操作できる最小 UI を配る。
- 第一目標: **技術者でない家族が、対象デバイスをスマホから確実に開閉できること。** リッチさより、軽さと「壊れない・嘘をつかない」を優先する。

---

## 絶対に守る設計原則

1. **プロトコルを直接喋らない。** バイト列・UDP・ポート 3610 を `mando` に持ち込まない。すべて `enl` / `casa` に委譲する。持ち込みたくなったら、それは層を間違えているサイン。

2. **バックエンド非依存。** 実行するコマンドは config のテンプレで決める。`enl → casa` の移行は**コード変更ではなく config の差し替え**で済むこと。`mando` 本体は `enl` と `casa` を区別しない、純粋な「コマンドテンプレを exec して JSON を返すサーバ」。

3. **フロントを下層から隔離する（最優先）。** フロントは `mando` の安定ミニ API だけを叩き、EPC も `enl` も `casa` も知らない。下層が変わってもフロントは壊れない＝**利用者の操作面が壊れない**。

4. **下層固有の知識は一点に閉じ込める。** `enl` の JSON（`properties[].value`）→ 「開 / 閉 / 不明」への**正規化関数だけ**がバックエンド固有。`casa` は出力スキーマが変わるので、移行時はここだけ差し替える。

5. **subprocess は直列化する。** `enl` は `0.0.0.0:3610` を専有 bind する。`casa` 経由でも `casa` が `enl` を呼ぶので透過的に同じ衝突が起きる。よって **exec 全体を `Semaphore(1)` で囲い、並行に走らせない**（axum は非同期だが、ここだけは意図的に直列）。

6. **状態は pull。** 下層は one-shot で状態を持たないので push する主体がいない。UI は 3〜5 秒ポーリングで state を取得する。INF 通知を拾うための常駐化は下層の思想に反するのでやらない。

7. **成否を正直に出す。** set は普通に失敗する（timeout / SNA / network）。**set 後は必ず state を取り直し、実際の開閉を確認してから表示**する。「閉じました」を楽観表示しない。`enl` の終了コードを明確な UI 状態へ写す:

   | code | 意味 | UI |
   |---|---|---|
   | 0 | success | 結果は state 再取得で確定 |
   | 3 | timeout | 「応答なし、もう一度」 |
   | 4 | device rejected (SNA) | 「機器が拒否」 |
   | 5 | network/bind failure | 「ネットワーク異常」 |

   > **light の例外:** light（特に mat wire group の groupcast）は無応答マルチキャストで、
   > 確認読み自体が代表ノード 1 台のプロキシ読みにすぎず確認として弱い。操作 POST は
   > 送信結果（`{"action": ...}`）のみ正直に返し、state は UI が押下 ~2 秒後に 1 回だけ
   > 非同期で追いつき取得するベストエフォート表示とする
   > （`docs/superpowers/specs/2026-07-10-light-async-state-design.md`）。
   > shutter は本原則どおり set 後の同期確認を維持する。

8. **`index.html` はバイナリに焼く（`include_str!`）、config は外に置く。** UI はプログラムの一部なので焼き込み、単一バイナリで配る。実 IP / EPC は設置環境ごとのデプロイデータで、再コンパイルなしに書き換えられるべき＝外出し。`casa` の「設定は外」原則とも揃う。成果物は **バイナリ 1 個 + `config.toml`**。

---

## やること（安定ミニ API）

- `GET  /` — 焼き込んだ `index.html` を返す
- `GET  /api/devices` — config 上の論理デバイス一覧
- `GET  /api/devices/{name}/state` — state テンプレを exec → 正規化 `{ "state": "open|closed|unknown", "raw": {...} }`
- `POST /api/devices/{name}/open` — open テンプレを exec → **直後に state 再取得** → 結果を返す
- `POST /api/devices/{name}/close` — 同上（close）
- `GET  /api/graphs` — config 上のグラフ一覧（きろくセクション）
- `GET  /api/graphs/{name}?period=today|week|month` — graph query テンプレを exec → 正規化した系列 `{ "series": [...] }`

---

## やらないこと

- プロトコル実装・名前解決ロジック（`casa` の責務）。
- スケジューリング・自動化・通知（外部オーケストレーションの責務）。
- 認証・リモート到達性。VPN / オーバーレイネットワーク等の**ネットワーク層に委譲**する。`mando` は bind するだけ（LAN なら LAN アドレス、外出先対応ならオーバーレイ網のアドレスに bind 先を変えるだけ。ポート開放不要）。
- 実 config（実 IP・EPC）のコミット。

---

## config（形）

論理デバイスごとに、操作 → 実行コマンド配列を持つ。本体はこの配列をそのまま exec するだけ。リポジトリには `config.example.toml` のみを置き、実値は各自がローカルで埋める。

```toml
[[device]]
name      = "shutter"
# 現状は enl。将来 casa に差し替えるだけ:
#   ["casa", "set", "shutter", "open_close_operation", "close"] など
get_state = ["enl", "get", "192.0.2.10", "026301", "open_close_state"]
open      = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "open"]
close     = ["enl", "set", "192.0.2.10", "026301", "open_close_operation", "close"]
```

> IP は RFC 5737 のドキュメント用レンジ（`192.0.2.0/24`）。実 IP に置き換えて使う。
> 実値（IP・open/close の hex・`open_close_state` の値域）は機種で振れるため、投入前に
> `enl describe <IP> 026301` で確認すること。電動シャッターは 0x0263。

---

## ネットワーク前提

- **LAN 内のみが MVP。** 外出先操作は VPN / オーバーレイネットワークでサービスのアドレスに到達させる（`mando` は bind 先を変えるだけ）。
- `mando` を動かすホストと対象デバイスは**同一 L2 / VLAN** にあること。コンテナで動かす場合はホストネットワーク（`network_mode: host`）が必要 — ブリッジネットワークでは UDP のデバイス応答とマルチキャストを受けられない。
- ポート 3610 は `enl` / `casa` が使う。同ホストで他の ECHONET Lite 実装が 3610 を握っていると応答を奪い合う。共存させない。

---

## ロードマップ

フェーズは原則「動くものを最短で利用者の手に届ける → 育てる」順。Phase 1 を満たした時点で実用に入る。

### Phase 1 — MVP（達成基準: 技術者でない家族が、対象デバイスをスマホから確実に開閉できる）

- [ ] `enl` 直叩きで単一デバイスの `get_state` / `open` / `close`
- [ ] set 後に state を再取得して結果を確定（楽観表示しない）
- [ ] `enl` 終了コード（3 / 4 / 5）→ UI 状態へのマッピング
- [ ] exec を `Semaphore(1)` で直列化
- [ ] `config.toml` 外出し + `index.html` 焼き込み = 単一バイナリ
- [ ] 大きいタッチターゲットのスマホ向け最小 UI（主操作を主役に、状態を明示）
- [ ] ホストネットワークのコンテナにデプロイ、LAN 内でスマホから動作確認

### Phase 2 — 複数デバイス & 操作面の磨き込み

- [ ] `config` の複数 `[[device]]` 対応 + UI でのデバイス選択（API は既に対応済み）
- [ ] ポーリング結果の短 TTL キャッシュ — 複数クライアントが 1 回の機器読み取りを共有し、3610 への負荷と直列待ちを抑える
- [ ] timeout 時の UI からの再試行導線

### Phase 3 — `casa` への移行（最上層を純粋な提供層に保つ）

- [ ] `config` を `casa` コマンドに差し替え（`enl` 直叩き → `casa` 経由）。本体コードは変更しない
- [ ] 状態正規化関数を `casa` の出力スキーマ対応に差し替え — **変更はこの一点に閉じること**
- [ ] `enl` 固有の前提が API / フロントに漏れていないことの確認（漏れていたら設計原則 3・4 違反）

### 自動的に効く拡張（個別作業を要さない）

- `casa` が `sbl` / `mat` 対応した暁には、`mando` は `config` にデバイスを追加するだけで新プロトコルに対応する。`mando` 本体の改修は不要 — これがバックエンド非依存設計の配当。

---

## 開発

```bash
cargo build --release      # → 単一バイナリ（index.html 焼き込み済み）
cargo test
cargo clippy -- -D warnings
RUST_LOG=debug cargo run
```

> 開発中に `index.html` を頻繁にいじる間は `tower-http` の `ServeDir` でディスクから配り、
> リリースで `include_str!` 焼き込みに切り替える二段構えも可。
