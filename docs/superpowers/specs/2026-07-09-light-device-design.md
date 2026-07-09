# ライトデバイス対応（mat 直叩き）設計

日付: 2026-07-09
状態: 承認済み

## 目的

`mat` で制御する Matter ライト（living_lights）を mando の UI から操作できるようにする。
操作は on / off / 色指定 / 色温度（kelvin）指定。バックエンドは一旦 `mat` 直叩き
（casa 経由は将来の config 差し替えで対応 — 本体コードは backend 非依存のまま）。

## 決定事項

1. **色・kelvin はプリセット方式。** config に完成したコマンド配列を名前付きで並べ、
   UI はボタンを出すだけ。任意値のユーザー入力 → exec 経路は作らない。
   「config のコマンド配列をそのまま exec する」原則を維持する。
2. **ライトは定期ポーリングしない。** mat 直叩きは chip-tool 起動で 1 コール数秒かかり、
   exec は全デバイス直列（Semaphore(1)）のため、ポーリングするとシャッター操作が
   詰まる。画面表示時に 1 回 + 操作後の state 再取得のみ（設計原則 7 は維持）。
   シャッターの 3〜5 秒ポーリングは従来どおり。
3. **デバイス種別 `kind` を導入。** `kind = "shutter" | "light"`、省略時 `"shutter"`
   （既存 config 完全互換）。

## config（形）

```toml
[[device]]
name  = "living_lights"
alias = "リビング照明"
kind  = "light"
get_state = ["mat", "read", "--node", "5", "--cluster", "onoff", "--attribute", "on-off"]
on    = ["mat", "on",  "--node", "5"]
off   = ["mat", "off", "--node", "5"]

[[device.preset]]
name  = "warm"
label = "電球色"
cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "2700"]

[[device.preset]]
name  = "daylight"
label = "白色"
cmd   = ["mat", "color-temp", "--node", "5", "--kelvin", "5000"]

[[device.preset]]
name  = "pink"
label = "ピンク"
cmd   = ["mat", "color", "--node", "5", "--name", "pink"]
```

### validate 規則

- `kind = "shutter"`（省略時）: `get_state` / `open` / `close` 必須（従来どおり）。
  `on` / `off` / `preset` の指定はエラー。
- `kind = "light"`: `get_state` / `on` / `off` 必須。`open` / `close` / `stop` の指定はエラー。
- preset: `name` 重複はエラー。`cmd` 空配列はエラー。`label` 任意（未指定なら name）。
- グループ: メンバーに `kind = "light"` を含むとエラー（グループは当面シャッター専用。
  必要になったら拡張）。

## API

既存に追加（既存エンドポイントは無変更）:

- `GET /api/devices` — 各要素に `kind` と `presets: [{name, label}]` を追加
  （shutter は `presets: []`）。
- `POST /api/devices/{name}/on` — on コマンド exec → state 再取得 → ActionView
- `POST /api/devices/{name}/off` — 同上（off）
- `POST /api/devices/{name}/presets/{preset}` — プリセット exec → state 再取得 → ActionView

kind が合わない操作（shutter への on、light への open 等）と未知のプリセット名は
404 + JSON エラー（既存の stop 非対応と同じ扱い）。

## 正規化（normalize.rs — 下層固有知識はこの一点に閉じる）

- `State` enum に `On` / `Off` を追加。
- `normalize_mat_onoff(raw)` を追加: mat read の出力
  `{"timestamp": "...", "node_id": 5, ..., "attribute": "on-off", "value": true}`
  の `value` を見て `true → On` / `false → Off` / それ以外 → `Unknown`。
- ディスパッチはデバイスの `kind` で行う（light → mat_onoff、shutter → enl）。
  casa 移行時はこの関数の中身だけ差し替える。

## exec（変更なし）

- mat は enl と同じ終了コード体系（0 = success / 3 = timeout / 4 = rejected /
  5 = network）を意図的に踏襲しているため、`ExecOutcome` のマッピングは無変更。
  mat 固有のコード（10/11/12/6 等）は既存の `Failed` に落ちる（stderr はログに出る）。
- Semaphore(1) の直列化も無変更。mat も同じゲートを通る
  （chip-tool の KVS 同時アクセス回避としても好都合）。

## UI（index.html — 焼き込み）

- light カード: 「つける」「消す」の大ボタン + プリセットのチップ列（横並びボタン）。
- 状態表示: 「点灯 / 消灯 / 不明」。画面表示時に 1 回取得、以降は操作後のみ更新
  （定期ポーリングなし）。
- shutter カードは従来どおり（ポーリング含め無変更）。
- 操作中はボタンを無効化し、結果は state 再取得の確定値で表示（楽観表示しない）。
- exec 失敗のユーザー向け文言は既存マッピングを流用
  （timeout →「応答なし、もう一度」等）。

## テスト

- config: kind パース / 省略時 shutter / light の必須フィールド / kind 不整合フィールドの
  拒否 / preset 重複・空 cmd の拒否 / light 入りグループの拒否。
- normalize: mat read 実出力形式の on / off / 想定外値 → unknown。
- API: on / off / preset 実行と 404 系（kind 不整合、未知 preset）。
  既存テストの回帰がないこと（`cargo test`）。

## やらないこと

- 色・kelvin の自由入力（スライダー / カラーピッカー）。プリセットで足りなくなったら再検討。
- ライトの定期ポーリング。
- ライトのグループ一括操作。
- matd 連携の前提化（config で `mat --matd ...` と書けば透過的に効くが、本体は関知しない）。
