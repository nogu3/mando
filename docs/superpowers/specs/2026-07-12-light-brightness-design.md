# light 明るさ（調光）制御 設計

日付: 2026-07-12
状態: 承認済み（実装は未着手 — 次に writing-plans → Subagent-Driven で実施）

## 背景 / 動機

light の色制御（`2026-07-10-color-sheet-arbitrary-color-design.md`）は色相・彩度のみで、
明度は「`mat group color` が明度を捨てる仕様」のため意図的に扱わなかった。
明るさ（調光）は色とは別コマンド（Matter Level Control 系）で操作でき、家族が
「もっと明るく／暗く」したい場面は日常的にある。そこで **明るさ制御を color と対称な
機構で追加する。**

## 決定事項

`color` と対称に、config テンプレ + `{brightness}` プレースホルダ + 専用スライダーで実装する。
下層コマンドは config が決め、mando 本体は検証済みの数値を argv 置換して exec するだけ
（バックエンド非依存を維持。casa 移行は配列差し替えのみ）。

### 値のセマンティクス

- スライダーは **1〜100（%）**。`{brightness}` に整数として代入される。
- 0 は扱わない（消灯は既存の off ボタン）。最小 1%。
- %→機器生値（例 0〜254）の変換が要る場合は config の mat 側で吸収する
  （mando は % のまま渡すだけ）。

### config

light の任意フィールド `brightness`（コマンドテンプレ配列）を追加。

```toml
# {brightness} が検証済みの整数 1〜100 に置換されて exec される
brightness = ["mat", "level", "--node", "5", "--percent", "{brightness}"]
```

検証（config 読み込み時、既存 `validate()` パターン）:
- `{brightness}` プレースホルダが配列全体でちょうど 1 個（0 個・2 個以上はエラー →
  新 `ConfigError::BrightnessPlaceholder`）
- 空配列はエラー（既存 color と同様）
- shutter に `brightness` があれば拒否（既存 `forbid()` に追加）

### API / サーバ

- **`POST /api/devices/{name}/brightness`**、JSON body `{"brightness": 50}`（整数）。
  - サーバ側で **1〜100 の整数**を厳密検証。範囲外・非整数（小数・文字列）は **400**
    （exec に到達させない）。値は argv 置換のみ（シェル非経由）。
  - 成功時の返りは light 例外どおり **`{"action":"<ExecOutcome>"}` のみ**（原則 7）。
  - `brightness` テンプレの無い device・shutter への POST は **404**（既存 color と同扱い）。
    未知 device も既存どおり 404。
- **`GET /api/devices` に `brightness_supported: bool` を追加**（device 単位）。
  UI はこれで明るさブロックの表示/非表示を決める。

### UI（既存の色シートを「あかり調整」シートに拡張）

- シートを開く条件に `brightness_supported` を追加:
  `presets.length || color_supported || brightness_supported`。
- シート先頭に **明るさブロック**: 「あかりの強さ」ラベル + スライダー 1 本
  （`<input type="range" min=1 max=100 step=1>`、既存 `.cslider` を流用）。
  `brightness_supported` が false ならこのブロックごと非表示。順序は
  明るさ → preset 行 → すきな色スライダー。
- 送信は既存の色スライダーと同じ **change イベントで 1 回だけ**（ドラッグ中の input では
  送らない → exec 直列 Semaphore(1) と相性を保つ）。新 `brightnessAct`
  （`colorAct` のパス違い）。busy 制御・エラー表示（ACTION_MSG）・~2 秒後の追いつき取得も
  既存どおり（明るさは state に出ないが on/off 鮮度維持として無害）。
- 初期つまみ位置は色スライダーと同様の **セッション記憶**（`lastBright`、デフォルト 100）。
  実際の明るさは state に出ないので現在値の主張はしない（原則 7）。
- シートを開くボタンのラベル/アイコンを **「💡 あかり調整」**に一般化する
  （現状「🎨 色を変える」。色専用でなくなるため固定で変更。対応内容に応じた出し分けはしない）。

## 実装ポイント

- `src/config.rs`:
  - `Device.brightness: Option<Vec<String>>`（serde default）
  - `brightness_cmd()` アクセサ（`color_cmd()` と対称）
  - `validate()` に `{brightness}` プレースホルダ検証・shutter 拒否を追加
  - `ConfigError::BrightnessPlaceholder { device, count }` を追加
- `src/main.rs`:
  - ルート `POST /api/devices/:name/brightness` 追加。body は
    `#[derive(Deserialize)] struct BrightnessReq { brightness: u8 }`
    （範囲は受信後に 1〜100 で検証、0 と 101〜255 は 400）。
  - 範囲検証 → テンプレの `{brightness}` を置換した argv を既存 executor に渡す →
    `LightActionView`（`{"action": ...}`）で返す。`color_device` と対称。
  - `DeviceInfo` に `brightness_supported: bool`。
- `index.html`: シートボタン文言を「💡 あかり調整」に、明るさブロック DOM/CSS
  （`.cslider` 流用）、`brightnessAct`、`lastBright` セッション記憶を追加。
  スライダーは `<input type="range">` を CSS で装飾（自前ドラッグ実装はしない）。
- デプロイ時: jarvis の実 config の light に `brightness` テンプレを追記
  （mat は絶対パス）。

## テスト / 検証

- config: `{brightness}` 0 個 / 2 個 / 空配列 / shutter 上の `brightness` が全部エラー、
  正常形がロードできる。
- API: 正常値 `50` → `{"action":"success"}`（sh 偽装 config で置換結果も確認できる形に）、
  `0` / `101` / `"50"`（文字列）/ 小数 → 400、テンプレ無し light / shutter / 未知 device
  → 404。`brightness_supported` が devices 一覧に出る。
- UI 実機: シートを開くと明るさスライダーが出る、離した時に明るさが変わる（~1 秒）、
  連続ドラッグでも送信は離した回数だけ、busy 中はスライダーも無効。ボタン文言が
  「💡 あかり調整」になっている。
- 既存の on/off・preset・color・shutter 経路が無変更で通ること（既存テスト全通過 + clippy）。

## やらないこと

- 明るさの state 読み戻し（light は on/off best-effort のまま。明るさは state に出さない）。
- 0% = 消灯 の UI 統合（最小 1%。消灯は既存 off ボタン）。
- 明るさ値の保存（localStorage・config 自動追記・「最近の明るさ」）。
- 色相/彩度と明るさの単一スライダー統合や HSV 一括送信（色と明るさは別コマンド・別送信）。
- shutter 側の変更一切。
