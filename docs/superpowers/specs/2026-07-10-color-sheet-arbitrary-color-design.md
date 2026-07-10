# 色選択ボトムシート + 任意色指定 設計

日付: 2026-07-10
状態: 承認済み（実装は未着手 — 次セッションで writing-plans → Subagent-Driven で実施）

## 背景 / 動機

タイル UI（`2026-07-10-ui-light-tiles-shutter-collapse-design.md`）の実機確認で
2 点のフィードバック:

1. **色玉（28px）がスマホで押しづらい。**
2. **任意の色も指定したい。** 来客が「この色もやってよ」と言う場面を想定。

調査（Apple Home / Google Home / Philips Hue）では 3 社とも
「カード内に色コントロールを詰めず、タップで専用ビュー（シート）を開く 2 段構え」が
共通パターン。モックで A 案（44px 色玉のタイル内完結）と D 案（ボトムシート）を比較し、
**D 案で合意**。

任意色は「その場限り」で合意 — 保存しない（localStorage も config 書き込みもしない）。
定番化したい色は従来どおり config の preset に追記する。

## 決定事項（UI）

1. **タイルの色玉行を「🎨 色を変える」ボタン 1 個に置換。**
   高さ 44px・タイル幅いっぱい。ボタン内に現在色ドット
   （= このセッションで最後に押した色。現在色の主張はしない — 原則 7）。
   preset も `color` テンプレも無い light はこのボタン自体を出さない。

2. **色選択はボトムシート。** ページに共有のシート要素 1 枚を置き、
   開くたびに対象 device の内容へ組み替える:
   - ヘッダ「{label} の色」
   - preset 行（高さ 48px、色ドット + 名前）。`.sel` リングは
     「セッションで最後に押した色」。preset が無ければこのブロックごと非表示。
   - 区切り「すきな色」の下に **虹（色相 0–360°）スライダー + こさ（彩度 0–100%）
     スライダー**。つまみ 44px。こさスライダーの右端色は現在の色相に追従させる。
     device に `color` テンプレが無ければこのブロックごと非表示。
   - 背景（dim）タップで閉じる。
3. **スライダーは離した時（change イベント）に 1 回だけ送信。**
   ドラッグ中（input イベント）は送らない — exec 直列（Semaphore(1)）と相性を保つ。
   HSV（V は常に 100%）→ hex に JS で換算して POST する。
   明度は変えない（`mat group color` は明度を捨てる仕様。UI にも明度は出さない）。
4. **送信経路は既存の light 機構をそのまま使う。** preset 行は既存 `presetAct`、
   任意色は新 `colorAct`（`lightAct` のパス違い）。busy 制御・エラー表示（ACTION_MSG）・
   ~2 秒後の追いつき取得も既存どおり（色は state に出ないが on/off 鮮度維持として無害）。

## 決定事項（config / サーバ）

5. **config: light の任意フィールド `color`（コマンドテンプレ配列）を追加。**

   ```toml
   # プレースホルダ {color} が検証済み hex（例 "#ff69b4"）に置換されて exec される
   color = ["mat", "group", "color", "--group", "living_lights", "--rgb", "{color}"]
   ```

   検証（config 読み込み時、既存 validate() パターン）:
   - `{color}` プレースホルダが配列全体でちょうど 1 個（0 個・2 個以上はエラー）
   - shutter に `color` があれば拒否（既存の kind 不整合フィールド拒否と同様）
   - mando 本体は下層コマンドを知らない（バックエンド非依存。casa 移行は配列差し替えのみ）

6. **API: `POST /api/devices/{name}/color`、JSON body `{"color": "#rrggbb"}`。**
   - サーバ側で `^#[0-9a-fA-F]{6}$` を厳密検証。不正は **400**（exec に到達させない）。
     値は argv 置換のみ（シェルを経由しない）で、検証済み hex 以外は渡らない。
   - 成功時の返りは他の light 操作と同じ **`{"action": "<ExecOutcome>"}` のみ**
     （原則 7 の light 例外どおり）。
   - `color` テンプレの無い device・shutter への POST は **404**（既存の
     kind 不整合と同じ扱い）。未知 device も既存どおり 404。
7. **`GET /api/devices` に `color_supported: bool` を追加**（device 単位）。
   UI はこれでスライダーブロックの表示/非表示を決める。preset の有無は既存
   `presets[]` で判る。

## 実装ポイント

- `src/config.rs`: `Device.color: Option<Vec<String>>`（serde default）+ validate() に
  プレースホルダ検証・shutter 拒否を追加。
- `src/main.rs`:
  - ルート `POST /api/devices/:name/color` 追加。body は
    `#[derive(Deserialize)] struct ColorReq { color: String }`。
  - hex 検証 → テンプレの `{color}` を置換した argv を既存 executor に渡す →
    `LightActionView`（`{"action": ...}`）で返す。
  - `DeviceInfo` に `color_supported: bool`。
- `index.html`: 色玉行 → colorbtn、シート DOM + CSS（dim / sheet / crow / hue / sat）、
  HSV→hex 換算、`colorAct`。スライダーは `<input type="range">` を CSS で装飾
  （自前ドラッグ実装はしない）。
- デプロイ時: jarvis の実 config の light に `color` テンプレを追記
  （`mat group color --group living_lights --rgb {color}`、mat は絶対パス）。

## テスト / 検証

- config: `{color}` 0 個 / 2 個 / shutter 上の `color` が全部エラー、正常形がロードできる。
- API: 正常 hex → `{"action":"success"}`（sh 偽装 config で置換結果も確認できる形に）、
  `#GGGGGG` や `red` や `#fff` → 400、テンプレ無し light / shutter / 未知 device → 404。
  `color_supported` が devices 一覧に出る。
- UI 実機: シート開閉、preset 行 48px、スライダー離しで色が変わる（~1 秒）、
  連続ドラッグでも送信は離した回数だけ、busy 中はシート内ボタンも無効。
- 既存の on/off・preset・shutter 経路が無変更で通ること（既存テスト全通過 + clippy）。

## やらないこと

- 明度（V）の操作・送信（mat が捨てる。UI にも出さない）。
- 任意色の保存（localStorage・config 自動追記・「最近使った色」）。
- OS 標準カラーピッカー（`<input type="color">`）— タブ・透明度が非技術者に難しい。
- preset の編集/追加 UI（config 直編集のまま）。
- shutter 側の変更一切。
