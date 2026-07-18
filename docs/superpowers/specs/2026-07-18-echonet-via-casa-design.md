# echonet（シャッター）を casa 経由に移行

**日付:** 2026-07-18
**状態:** 設計合意済み → 実装計画へ

## 背景と目的

ロードマップ Phase 3「`casa` への移行」の第一歩。casa が jarvis 上で echonet
（照明・solar・power_meter）を扱えるよう設定済みになったのを受け、mando の
**echonet デバイス（シャッター 5 台）の exec を `enl` 直叩きから `casa` 経由に
切り替える**。

設計原則2（バックエンド非依存）・3（フロントを下層から隔離）・4（下層固有知識を
一点に閉じ込める）の実地検証を兼ねる。狙いは「mando 本体コードの変更を正規化関数
一点に閉じ、残りは config 差し替えで済む」ことを実証すること。

## スコープ

**対象: echonet デバイス（シャッター 5 台 shutter1..5 / 192.168.1.222 /
eoj 0x026301..05）のみ。**

- 対象外: matter ライト 2 台（living_lights / desk_light）。casa の matter 対応が
  mando の使う group / color / color-temp / brightness を同等に賄えるか未検証のため、
  mat 直叩きのまま据え置く。matter の casa 化は別タスク。
- 対象外: exec 直列化・set 後 state 再取得・終了コードマッピング・ポーリング窓・
  グラフ / health。いずれも変更不要。

## 現状の確認結果（実機調査済み）

- **casa の get 出力は enl 出力を `value` でラップする**:
  ```json
  {"device":"porch_light","protocol":"echonet","timestamp":"...",
   "value":{"eoj":"029102","esv":"GetRes","ip":"...",
            "properties":[{"epc":"80","name":"power","value":{"power":"on"}}]}}
  ```
  enl 直の出力は `value` ラッパーが無く `properties` がトップレベル。
- **casa の set は echonet アダプタが素通しで enl へ写す**:
  `casa set <name> <property> <value>` → `enl set <ip> <eoj> <property> <value>`
  （casa-core `adapter/echonet.rs` で確認）。値の検証はせず enl に委譲。
- **casa は `~/.local/bin/casa`** にあり、mando サービスの PATH
  （`/home/jarvis/.cargo/bin:/usr/local/bin:/usr/bin:/bin`）に無い。
  enl は `/usr/local/bin/enl` で PATH 内のため bare で動いていた。
- **shutter は casa 未定義**。`~/.config/casa/devices.toml` に照明・solar・
  power_meter はあるがシャッターが無い。casa 経由化にはまず casa 側に追加が必要。
- casad（常駐）との 3610 共存は enl 1.5.0 で解決済み。mando は get/set のみで
  INF listen を使わないため、casa を挟んでも新たな衝突は生じない。

## 変更内容

### 1. mando 本体コード（唯一のコード変更・設計原則4）

`src/normalize.rs` の `normalize_enl_state` を **casa エンベロープ対応**にする。

- 関数冒頭で内側オブジェクトを解決する:
  「`raw["value"]` が `properties` を持つオブジェクトなら、それを内側とみなす。
  そうでなければ `raw` 自身を内側とする」。
- 以降の処理（`properties` 配列から `open_close_state` を探し、`value.state` を
  分類する `classify`）は**一切変更しない**。
- 結果として **casa 経由・enl 直の両方を受ける**。config.example に残す enl 例が
  壊れず、将来の切り戻しも安全。
- enl 直の出力はトップレベルに `value` を持たないため、判定は曖昧にならない。

テスト追加（`normalize.rs` の `#[cfg(test)]`）:
- casa エンベロープ正常系: `{"device":..,"protocol":"echonet","value":{"properties":
  [{"name":"open_close_state","value":{"state":"fully_closed"}}]}}` → `Closed`。
- casa エンベロープで全 5 状態（fully_open/closed/opening/closing/stopped_midway）。
- `value` はあるが `properties` を持たない（例: `{"value":{"power":"on"}}` 相当の
  非対象スキーマ）→ 従来どおり `Unknown` に落ちること。
- 既存の enl 直テストは全て通過し続けること（後方互換の担保）。

これ以外のコード（exec.rs / main.rs / config.rs）は変更しない。

### 2. リポジトリ config.example.toml

シャッター例を **casa 形をリードに**書き換える:
```toml
get_state = ["casa", "get", "shutter", "open_close_state"]
open      = ["casa", "set", "shutter", "open_close_operation", "open"]
close     = ["casa", "set", "shutter", "open_close_operation", "close"]
stop      = ["casa", "set", "shutter", "open_close_operation", "stop"]
```
enl 直の形も併記し「正規化が両対応なのでどちらも有効」と注記。挙動には影響しない
ドキュメント整合のみ。mat ライト・グラフ・health のサンプルは変更しない。

### 3. jarvis 実機設定（リポジトリ外・デプロイデータ）

**(a) casa にシャッターを追加** — `~/.config/casa/devices.toml`:
```toml
[devices.shutter1]   # リビング
protocol = "echonet"
ip = "192.168.1.222"
eoj = "0x026301"
# shutter2 = 0x026302（南側FIX）, shutter3 = 0x026303（西側FIX）,
# shutter4 = 0x026304（腰窓南）, shutter5 = 0x026305（腰窓北）
```
既存の `.bak` 運用に倣い編集前にバックアップを取る。追加後 `casa list` に 5 台が
出ること、`casa validate`（あれば）が通ることを確認。

**(b) casa を /usr/local/bin へ install** — mando サービスの PATH に入れ bare
`casa` で呼べるようにする。enl と同じ場所に揃う。既存の `~/.local/bin/casa`
（現行 0.7.1）を `sudo install -Dm755` で `/usr/local/bin/casa` へ配置する
（ビルドし直さず既存バイナリをそのまま使う）。install 後 `/usr/local/bin/casa
--version` が 0.7.1 を返すこと。

**(c) mando config のシャッター 5 台を casa に差し替え** —
`/etc/mando/config.toml`:
```toml
get_state = ["casa", "get", "shutter1", "open_close_state"]
open      = ["casa", "set", "shutter1", "open_close_operation", "open"]
close     = ["casa", "set", "shutter1", "open_close_operation", "close"]
stop      = ["casa", "set", "shutter1", "open_close_operation", "stop"]
```
shutter2..5 も device 名を対応させて同様に。mat ライト 2 台・グラフ・health は
変更しない。編集前に config.toml をバックアップ。

## デプロイ順序

正規化が両エンベロープ対応なので順序の制約は弱いが、切り分けやすい順で進める:

1. **jarvis casa 側**: devices.toml にシャッター追加 + casa を /usr/local/bin へ
   install → `casa get shutter1 open_close_state` で疎通確認。
   この時点で mando はまだ enl 直＝稼働したまま。
2. **mando 新バイナリ**: normalize 変更を含むバイナリをビルド・デプロイ
   （[[jarvis-mando-deploy-path]] の手順: ローカルから直接 push → ff → build →
   install → restart）。config はまだ enl のままでも両対応なので稼働継続。
3. **mando config 差し替え**: config.toml のシャッターを casa 形に → mando restart
   → 再確認。

## 検証

- **ローカル**: `cargo test`（casa エンベロープの新テスト含む全通過）+
  `cargo clippy -- -D warnings`。
- **jarvis 疎通（非破壊）**:
  - `casa get shutter1 open_close_state` が casa エンベロープを返す。
  - mando `GET /api/devices/shutter1/state` が正規化 state（open/closed/...）を返す。
  - 5 台すべてで state 取得が成功する。
- **実開閉（破壊的）**: open/close/stop は物理的にシャッターが動くため自動実行しない。
  get 系の疎通確認まで進めた上で、実開閉テストは実施前にユーザーへ確認してから
  1 台で open→state 再取得→close を手動検証する。

## 未対応・将来

- matter ライトの casa 化（別タスク。casa の matter 動詞カバレッジ検証が前提）。
- casa の /usr/local/bin install を jarvis-iac（Ansible）で恒久管理するか否かは
  本タスクの対象外（今回は手動 install）。

## 関連

- ロードマップ Phase 3（CLAUDE.md）
- [[jarvis-mando-deploy-path]] — デプロイ手順
- [[casad-holds-3610]] — 3610 共存の前提
