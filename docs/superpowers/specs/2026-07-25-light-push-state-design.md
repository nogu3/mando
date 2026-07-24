# light 状態の push 化（mat listen → SSE）設計

日付: 2026-07-25
状態: 承認済み（実装は未着手 — 別セッションで writing-plans → Subagent-Driven で実施）
前提文書: `2026-07-10-light-async-state-design.md`（本書はその追いつき取得を置き換える）

## 背景 / 動機

「ライトを点けると物理は即点くのに、UI の状態表示が遅い／応答なしになる」の調査
（2026-07-24, jarvis 実機）で真因が確定した。

- light の on/off・色・明るさは **groupcast（無応答マルチキャスト）**。応答を待たないので
  物理は即反応する。**unicast なのは確認 read だけ**。
- 確認 read は代表ノード（`living_lights` → node 5 / `desk_light` → node 6）への `mat read`。
  これが **CASE セッションの cold-start** を踏むと極端に遅い／失敗する。

実測値:

| 条件 | 結果 |
|---|---|
| 未購読ノードの初回 read | **3.6s（node5）/ 80.7s（node6）**、`CASE ... exchange error: no acknowledgement`（exit 6）も発生 |
| 2 発目以降 | **~100ms** |
| 購読確立後、**2 時間以上 idle** を挟んだ read | **159ms（warm のまま）** |

つまり cold-start は「**matd がまだ購読していないノードの初回**」だけで起き、
一度購読が確立すれば heartbeat がセッションを生かし続け、idle では失効しない。
mando の `[exec] timeout_ms`（既定 15000）が長い cold handshake を打ち切ることで
UI の「応答なし」になっていた。

さらに `mat listen` が **priming:false のライブ変化イベントを push** することを実測で確認した
（ライト off で代表 node 5 の `onoff/on-off=false` が実際に降ってきた）。
すなわち **read をやめて push で状態を得れば、cold-start は原理的に消える**。

これは症状緩和ではなく根治である。本設計はそのための pull → push 移行。

### 競合しないことの確認（実測）

- `casad`（常駐ルールエンジン）が listen しているのは **ECHONET INF（UDP 3610）**。
  `matd` が扱うのは **Matter（Thread）**。別トランスポート・別デーモンで、
  `ss` でも casad は matd socket に接続していない。**チャネルを共有しない**。
- `mat listen` は **複数クライアントへファンアウトする**（2 本同時接続して両方生存を実測）。
  mando が listener を 1 本増やしても他を奪わない。
- CLAUDE.md 原則 6 の「INF 通知のための常駐化はしない」は **echonet 限定**の話。
  Matter は matd が常駐 Subscribe を持つのが mat ファミリの設計なので、
  mando が listen に乗るのは下層の思想に**沿う**。この非対称は意図的。

## 決定事項

1. **今回は on/off だけ push で反映する。** 明るさ・色の値表示（以下「B」）は将来やるが、
   本設計のスコープ外。ただし **B を「属性を足すだけ」で実現できる形**にしておく
   （後述の汎用マップ）。listener・突合・SSE・復旧の仕組みは B でも変更不要とする。
2. **push を主・read をフォールバックにする（正直さの三段構え）。**
   primed なら in-memory 即答、unprimed／listener 断なら read で確定、
   それも失敗なら `stale: true` で正直に出す。
3. **ブラウザまで push する（SSE）。** cross-tab / 別端末の操作もライブで全画面に反映する。
   一方向の状態配信であり、client→server は既存 POST のままなので WebSocket の
   双方向性は不要。EventSource が自動再接続を持ち、追加依存なし・axum ネイティブ対応で
   単一バイナリの思想に合う。
4. **node_id は config に持たせる。** 突合キーはイベント側のキー名と揃えて `node_id`。
5. **`[push]` セクションは任意。** 未設定なら push 機能は完全に無効で、既存挙動と変わらない。

### なぜ node_id を config に置くのか（read から学習しない理由）

listen イベントは `{"node_id": 5, ...}` と数値で来るが、jarvis の config は
`--node living_south_tape_light` と **alias 表記**で、alias→node_id の対応表は
mat の `aliases.toml` にあり mando は持たない。この突合をどう解くかで 3 案あった。

採用しなかった案と理由:

- **read の戻り値から node_id を学習する**: read の JSON には `node_id` が入るので
  自己修復的に覚えられる。しかし**学習には read 成功が要り、その read こそが
  cold で 80s／失敗するもの**。今回直したい失敗モードが、そのまま学習の失敗モードになる。
- **`get_state` の `--node` を parse する**: jarvis は alias 表記なので数値が取れず、
  Phase 3（casa 化）で `--node` が消えると壊れる。
- **mat 側に listen で alias も出させる**: mat リポジトリの変更＋リリースに mando が縛られる。
  また matd の購読は **node_id が一次キー**で、alias を持たないノードも存在しうるため
  **結局 node_id で引く道が必要**。good-to-have だが前提条件にはしない。

node_id は「機器を設置・commission した結果決まるデプロイデータ」であり、
CLAUDE.md が config に置くと定めている **実 IP・EPC と同じクラス**のもの。
config に置くのが原則 2 に素直で、突合が起動時から確定し、テストも純関数にできる。

唯一の弱点である**再 commission 時のドリフト**は、read 成功時に戻り値の `node_id` と
config 値を突き合わせ、**不一致なら warn を出す**ことで「黙って壊れない」ようにする
（自己修復はしない — 設定の真実は config である、という一貫性を優先）。

## 鮮度モデル（本設計の肝）

**push 値を TTL で腐らせない。** 静止したライトの状態は勝手に変わらず、変われば
イベントが来る。よって値の「古さ」は状態の正しさと無関係で、信頼できるかどうかは
**listener が生きているか**だけで決まる。既存の `[cache] state_ttl_ms`（shutter 用の
short-TTL キャッシュ）とは別の考え方であり、light は引き続きあのキャッシュを通さない。

デバイスごとに 3 状態を持つ:

| 状態 | 条件 | GET state の挙動 |
|---|---|---|
| **primed** | listener 接続中 かつ 基準値確立済み | push 値をそのまま返す。**exec ゼロ・即答** |
| **unprimed** | 起動直後／再接続直後で基準値が未確立 | **1 回 read** して確定（＝購読の誘発も兼ねる） |
| **disconnected** | listener 断・再起動待ち | read フォールバック。失敗なら `stale: true` |

**再接続時は全 light を unprimed に落として read で再ベースラインする。**
切れていた間に状態が変化した可能性があるうえ、`mat listen` は
**新規クライアント接続に priming を replay しない**（実測: 新規 listen 3 秒で 0 件）。
よって再 read が唯一の正しい復旧手段であり、これが同時に購読を誘発して
cold-start を解消する。

## アーキテクチャ

新規ユニットは 3 つ。既存の exec / cache 経路には手を入れない。

| ユニット | 責務 | 置き場所 |
|---|---|---|
| **listener task** | `mat listen` を長寿命サブプロセスとして起動し stdout を 1 行 1 JSON で読み続ける。落ちたら backoff 再起動 | `src/push.rs` |
| **PushStore** | node_id → 論理デバイスの突合と、デバイスごとの最新属性値を in-memory 保持 | `src/push.rs` |
| **broadcast** | 状態変化を全 SSE クライアントへ扇形配信（`tokio::sync::broadcast`） | `src/push.rs` |

**listener は `run_bounded` を通さない。** あれは one-shot exec 用の
「レーン直列化 + 15s timeout」であり、無期限ストリームに適用すると即座に打ち切られる。
listen は matd 経由で 3610 を掴まないためレーンも不要（CLAUDE.md 原則 5 の
「mat は matd が並行を捌くのでレーン不要」と同じ理由）。

**下層固有知識の閉じ込め（原則 4）:** イベント JSON のパース
（`node_id` / `cluster` / `attribute` / `value` の取り出し）は Matter 固有なので
**`normalize.rs` に置く**。`push.rs` は「行を受け取り正規化関数に渡し、store と
broadcast を更新する」だけの下層非依存な機械にする。casa 移行時に差し替わるのは
`normalize.rs` の関数だけ、という既存の構造を保つ。

### PushStore の形（B への拡張性）

デバイスごとに `(cluster, attribute) → value` の**汎用マップ**を持つ。
今回 state に写すのは `onoff/on-off` のみ。

B（明るさ・色）のときは **`levelcontrol/current-level` や `colorcontrol/*` を
読み出して view に足すだけ**で、listener・突合・SSE・復旧の仕組みは変更不要。
実測で同じストリームに `current-level` も `color-temperature-mireds` も流れてくることを
確認済み。

## config

```toml
[push]                                   # セクションごと任意。無ければ現状動作のまま
listen = ["mat", "listen", "--timeout-ms", "0"]   # 0 = 無期限

[[device]]
name  = "living_lights"
kind  = "light"
node_id = 5                              # push イベントの突合キー
get_state = ["mat", "read", "--node", "living_south_tape_light", "--cluster", "onoff", "--attribute", "on-off"]
```

- 実行するコマンドは config が決める（原則 2）。本体は `mat` を名指ししない。
- `[push]` 未設定 → push 機能は完全無効。既存挙動と 1 バイトも変わらない。
- `kind = "light"` で `node_id` が無いデバイスは、**そのデバイスだけ**従来の read 経路の
  ままとし、起動時に warn を出す（設定漏れを黙って無視しない）。
- `node_id` は light 以外の kind では無視する（shutter は本設計の対象外）。
- `config.example.toml` に `[push]` と `node_id` の例・コメントを追記する。

## API

| エンドポイント | 変更 |
|---|---|
| **`GET /api/events`** | **新規（SSE）**。接続直後に全 light の現在スナップショットを 1 発送り、以後は変化のたびにイベント |
| `GET /api/devices/{name}/state` | light のみ push 優先に変更。`source: "push"｜"read"` と `stale: bool` を追加 |
| 操作系 POST（on/off/presets/color/brightness） | **無変更**（送信結果のみ返す現行仕様を維持） |
| shutter / group / graphs / health / mesh 系 | **無変更** |

`/api/events` のイベント形（light のみ流す）:

```json
{"device": "living_lights", "state": "on", "source": "push", "stale": false}
```

接続直後のスナップショットも同じ形で全 light 分を送る（新しく開いたタブが即座に正しくなる）。
`source` / `stale` を出すのは、UI が「いま何を根拠に表示しているか」を隠さないため（原則 7）。

API バージョニングはしない（利用者は同梱 UI のみ）。

## エラー処理 / 復旧

| 事象 | 挙動 |
|---|---|
| listener プロセス死亡 / matd 不在（exit 13） | 指数 backoff（1s → 30s 上限）で再起動。全 light を unprimed に落とす |
| 未知 node_id のイベント | 無視（debug ログのみ）。家には mando 管理外の Matter ノードが多数いる（実測: node 7, 8, 9, 10, 14 …） |
| 壊れた JSON 行 | その行だけ drop してストリームは継続（部分的な破損で全体を落とさない） |
| **node_id ドリフト** | read の戻り値 `node_id` が config と不一致なら **warn**（再 commission を黙って壊さない） |
| 再ベースラインの read が失敗 | そのデバイスは unprimed のまま。GET state は read フォールバック → 失敗なら `stale: true` |
| SSE 接続断 | ブラウザの EventSource が自動再接続。切れている間はクライアントが従来の追いつき取得にフォールバック |

**起動時**: listener を spawn し、全 light の再ベースライン read を**非同期で**走らせる
（起動をブロックしない。read は既存の executor / lane / timeout の枠内で行う）。

## クライアント（index.html）

- `EventSource("/api/events")` を張り、light の状態イベントで **既存の `renderState` を呼ぶ**
  （描画ロジックは再利用。`source` / `stale` の扱いも `renderState` 側に寄せる）。
- SSE 接続中は `scheduleLightCatchup` を**張らない**。接続が切れていれば従来どおり張る
  ＝ degradation が正直に効く。`LIGHT_CATCHUP_MS` は fallback 用として残す。
- **shutter のアクティブ窓ポーリングには一切触らない**（`2026-07-18-active-window-polling-design.md`
  の挙動を維持）。SSE が運ぶのは light のみ。

## テスト / 検証

**単体（cargo test）:**

- `normalize`: listen イベント JSON → 構造体。異常系（`value` 欠落・型違い・未知 cluster・壊れた行）。
- `PushStore`: node_id 突合、未知ノードの無視、再接続で全 light が unprimed 化されること。
- **フォールバック分岐**: primed のとき **exec が呼ばれないこと**をアサートする
  （push の価値そのものの証明）。unprimed のとき read が 1 回走ること。
- `config`: `[push]` 無し＝既存挙動、`node_id` 無し light の degrade（warn しつつ read 経路）。
- SSE: router を oneshot で叩き、接続直後スナップショット → 変化イベントの順で届くこと。

**e2e（jarvis 実機）:**

- mando 起動後、`GET /api/devices/living_lights/state` が `source: "push"` で **~即答**すること。
- 別端末（or curl）で on → **開いている UI がポーリングなしで反映**されること。
- matd を再起動 → listener が再接続し、再ベースライン後に再び `source: "push"` へ戻ること。
- 検証は夜間を避け、終了時は必ず元の on/off 状態へ戻す。

`cargo clippy -- -D warnings` がクリーンであること。

## やらないこと

- **明るさ・色の値表示（B）** — 将来やるが本設計のスコープ外。store は拡張可能な形にしておく。
- **shutter の push 化** — shutter は ECHONET で push の主体がおらず、原則 6/7 のまま
  （set 後の同期確認 + アクティブ窓ポーリング）を維持する。
- **push 値の永続化** — mando 再起動で unprimed から始めてよい（mesh スナップショットと同じ割り切り）。
- **mat 側への alias 出力要求** — 独立した ergonomics 改善として別途検討しうるが、
  本設計はこれに依存しない。
- **WebSocket 化 / API バージョニング**。

## 未確認事項（実装時に確認する）

- **matd 再起動後に全 light 代表ノードを自動再購読するか。** 未確認だが、本設計は
  「再接続時に必ず再ベースライン read を打つ」ため、自動再購読の有無にかかわらず
  購読が誘発され、動作は成立する。挙動の差は「初回 read が cold で遅いかどうか」だけ。
- **購読 drop からの復旧**。イベントに `recovered` フラグがあり matd 側に再購読ロジックは
  あると見られるが、実 drop → recover サイクルは未観測。mando 側は listener 断の検知と
  再ベースラインで吸収する設計なので、前提にはしない。
