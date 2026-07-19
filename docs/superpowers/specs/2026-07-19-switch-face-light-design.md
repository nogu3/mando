# 設計: switch の `face = "light"`（on/off 照明を照明らしく見せる）

## 背景と目的

`kind = "switch"`（enl/casa の on/off 機器）は用途を問わず「🔌 スイッチ」セクションに
⏻ アイコンのトグルタイルで表示される。だが実際の switch には照明（casa の 0x0291
一般照明など、on/off だけの照明）も含まれ、それらが「スイッチ」の下に ⏻ で並ぶのは
家族向け UX として不自然。照明は照明（💡・照明セクション）として見せたい。

switch は「見た目は light、振る舞いは shutter（同期確認）」の on/off 汎用 kind なので、
kind は switch のまま、**表示の面（アイコン・セクション・ラベル）だけを config で
切り替える**。下層は一切関与しない（設計原則3: 表示は config 駆動、フロントは mando の
安定 API だけを見る）。

## スコープ

**やること:**
- switch device に任意フィールド `face` を追加。値は `"light"` のみ。
- `face = "light"` の switch を、💡 アイコン・「💡 照明」セクション・「点灯/消灯」ラベルで表示。
- 振る舞い（on/off・set 後同期確認・アクティブ窓ポーリング）は switch のまま不変。
- jarvis 実機の 5 照明に `face = "light"` を付与して再適用。

**やらないこと（YAGNI）:**
- 汎用のセクション定義機構（`section`/`icon` 自由指定）。
- `face` の `light` 以外の値（fan 等の専用フェイスは必要になってから）。
- light-faced switch への色・明るさ・プリセット（switch は純 on/off のまま）。
- kind = shutter / light の表示変更。

## 設計

### config.rs

- `Face` enum を追加（`#[serde(rename_all = "snake_case")]`、現状 `Light` の 1 バリアント）。
  Deserialize + Serialize + Copy + PartialEq。未知の値は serde が parse エラーにする。
- `Device` に `face: Option<Face>`（`#[serde(default)]`）を追加。
- 検証:
  - `Kind::Switch` アーム: `face` は任意（追加検査なし。指定するなら enum 妥当性は parse 時点で担保）。
  - `Kind::Shutter` / `Kind::Light` アーム: `face` を禁止する。`d.face.is_some()` なら
    `ConfigError::ForbiddenField { field: "face" }` を返す（既存の forbid 系と同じ扱い）。

### main.rs

- `DeviceInfo` に `face: Option<Face>` を追加し、`list_devices` で `d.face` を載せる。
  `#[serde(skip_serializing_if = "Option::is_none")]` は付けず、`Option` はそのまま
  `null`/値で出す（フロントは `dev.face === "light"` で判定）。
- 状態正規化・操作ルーティングは変更なし（face は表示専用。switch は既に
  `run_action` 同期パス＋ポーリング対象）。

### index.html

`face` は `/api/devices` の各要素に出る（`"light"` または `null`/不在）。

1. **`buildSwitchTile(dev)`**: グリフを出し分ける。
   - `dev.face === "light"` → `💡`、それ以外 → `⏻`。aria-label も「点灯/消灯」/「オン/オフ」。
   - カードに `face: dev.face` を保存（ラベル選択に使う）。
2. **`renderState`**: ラベルマップを face 対応にする。
   - `const labelMap = (c.kind === "switch" && c.face !== "light") ? SWITCH_LABEL : STATE_LABEL;`
   - → 素の switch は「オン/オフ」、light-faced switch は STATE_LABEL の「点灯/消灯」。
     light・shutter は従来どおり STATE_LABEL。
3. **`boot()` レイアウト分岐**:
   - `lights = kind==="light"`、`switchLights = kind==="switch" && face==="light"`、
     `plainSwitches = kind==="switch" && face!=="light"`、`shutters = kind==="shutter"`。
   - 「💡 照明」セクション: `lights.length || switchLights.length` があれば表示。
     同一 `.tiles` グリッドに mat ライト（`buildLightTile`）→ 次に switch 照明
     （`buildSwitchTile`）の順で並べる。
   - 「🔌 スイッチ」セクション: `plainSwitches.length` があれば表示（無ければ非表示）。
   - shutter セクションは不変。
   - light-faced switch も switch なので、ポーリング（`pollOnce` は light のみ除外）・
     同期パス（`deviceAct` は light のみ早期 return）に自動的に乗る。初期取得も
     `refreshOnce`→`pollOnce` が拾う（`fetchLightStatesOnce` は kind==="light" 限定なので素通り）。

light-faced switch のタイルは既存 `.tile`/`.lit`/`button.bulb` CSS を再利用（新規 CSS なし）。
💡 が amber の `.lit` グローで点灯するのは mat ライトタイルと同じ見た目。

### config.example.toml

switch の例に `face = "light"`（任意）の説明を 1〜2 行追記する。

### jarvis 実機（別ステップ・構成変更なので jarvis-iac 経由）

`roles/mando/files/config.toml` の 5 照明
（entrance_indirect_light / hallway_floor_light / kitchen_light / washstand_light /
wic_downlight）に `face = "light"` を追加し、`ansible-playbook` で再適用 → mando restart。

## テスト

### config.rs（ユニット）
- switch + `face = "light"` が parse され `face == Some(Face::Light)`。
- switch で `face` 未指定なら `face == None`。
- shutter に `face = "light"` → `ForbiddenField { field: "face" }`。
- light に `face = "light"` → `ForbiddenField { field: "face" }`。
- switch に未知の `face`（例 `"fan"`）→ parse エラー（`ConfigError::Parse`）。

### 手動確認
- `cargo build --release` / `cargo test` / `cargo clippy -- -D warnings`。
- switch + face=light を含む config で起動し、`/api/devices` に `"face":"light"` が出る／
  UI で当該デバイスが「💡 照明」セクションに 💡 タイルで並ぶ／タップで on/off が
  同期確認され「点灯/消灯」表示になる／ポーリングされる、を確認。
- face 無しの switch は「🔌 スイッチ」に ⏻ で残ることを確認。

## 変更ファイル一覧

| ファイル | 変更 |
|--|--|
| `src/config.rs` | `Face` enum、`Device.face`、shutter/light アームで face 禁止、テスト |
| `src/main.rs` | `DeviceInfo.face` を公開 |
| `index.html` | `buildSwitchTile` グリフ出し分け、`renderState` ラベル face 対応、`boot()` レイアウト分岐 |
| `config.example.toml` | switch 例に `face` の説明追記 |
| （別ステップ）jarvis-iac `roles/mando/files/config.toml` | 5 照明に `face = "light"` |

## 設計原則との整合

- **原則3（フロント隔離）**: フロントは `kind` と `face` と on/off/unknown しか見ない。EPC も casa も知らない。
- **原則8（config は外・表示は config 駆動）**: 表示の面を config の 1 フィールドで切り替える。
- 正規化・直列化・pull・正直な成否（原則4〜7）は switch のまま不変。face は純表示。
