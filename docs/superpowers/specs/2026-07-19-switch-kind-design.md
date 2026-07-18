# 設計: `kind = "switch"` — enl の on/off 機器を UI に追加する

## 背景と目的

現状 mando が UI に出すのは shutter（開閉）/ light（mat の on/off）/ グラフ / health。
利用者は **enl 経由の別 ECHONET 機器**（換気扇・床暖房 on/off・エアコン電源など、
電源 on/off だけで操作する機器）も UI から操作したい。

これらはユニキャストで確実に状態を読めるため、mando の設計原則7「set 後は必ず
state を取り直し、実際の変化を確認してから表示する」を効かせられる。既存の
`kind = "light"` は **mat 用かつマルチキャスト前提**で、状態正規化が mat スキーマ
専用・確認読みが best-effort。enl の on/off 機器にはこの前提が合わない。

そこで **新 `kind = "switch"`** を足す。light の multicast 事情と混ざらない、
「enl の on/off 機器専用・同期確認あり」の系統として実装する。

## スコープ

**やること:**
- `kind = "switch"` の追加（config 検証・状態正規化・API ルーティング・UI）
- switch は on/off のみ。set 後に同期で state 再取得。アクティブ窓でポーリング。
- UI は light と同じ**タイル**（大きなアイコンをタップで on/off トグル）。ただし
  振る舞いは shutter 系（ポーリング＋同期確認）。
- config.example.toml に switch の例を追記。

**やらないこと（YAGNI）:**
- switch のグループ一括操作（当面シャッター専用の開閉意味論に合わない）。
- 色・明るさ・プリセット・stop（switch は純 on/off）。
- enl プロトコルの持ち込み（設計原則1。従来どおり config のコマンド配列を exec するだけ）。

## 3 kind の対比

| | shutter | light | **switch（新）** |
|--|--|--|--|
| 操作 | 開/閉/(停) | on/off/色/明るさ/プリセット | **on/off のみ** |
| 状態正規化 | `normalize_enl_state` | `normalize_mat_onoff` | **`normalize_enl_state`（on/off も読めるよう拡張）** |
| set 後確認 | 同期（`run_action`） | best-effort（`run_light_action`） | **同期（`run_action`）** |
| ポーリング | アクティブ窓 | しない | **アクティブ窓** |
| UI | カード（開/閉ボタン） | タイル（アイコン+シート） | **タイル（アイコンをタップでトグル）** |
| グループ | 可 | 不可 | **不可** |

要点: switch は **見た目は light（タイル）、振る舞いは shutter（同期確認＋ポーリング）**。

## バックエンド設計

いずれも既存の kind 分岐に一項足すだけ。新しい実行系統は作らない。

### 1. config.rs — `Kind::Switch`

- `Kind` enum に `Switch` を追加（`#[serde(rename_all="snake_case")]` なので TOML では `"switch"`）。
- `validate()` に Switch のアームを追加:
  - **必須:** `on`, `off`（`require`）。`get_state` は全 kind 共通で既に必須。
  - **禁止:** `open`, `close`, `stop`, `color`, `brightness`, `preset`（`forbid` / presets 非空チェック）。
- グループメンバー検証: 現状 `Some(d) if d.kind == Kind::Light` を拒否している。これを
  **shutter 以外を拒否**する形に一般化する（`d.kind != Kind::Shutter` を拒否）。
  エラーは既存の `LightInGroup` を `NonShutterInGroup { group, member }` にリネームし、
  メッセージを「group にはシャッターのみ入れられる」に更新する。
  - 理由: これで light も switch も同じ一点で弾け、将来 kind が増えても
    「グループはシャッター専用」の不変条件が 1 箇所に閉じる。

### 2. normalize.rs — `normalize_enl_state` を on/off 対応に拡張

`normalize_enl_state` に operation_status（電源 on/off）の分類を足す。開閉と on/off の
両方を **同じ enl 正規化関数**が受ける（設計原則4: enl 固有知識はこの一点に閉じる）。

- `classify_str`: `"on"` → `State::On`、`"off"` → `State::Off` を追加。
- `classify` の数値アーム: `0x30` → `On`、`0x31` → `Off` を追加
  （ECHONET Lite operation_status EDT: 0x30=ON / 0x31=OFF）。
- オブジェクト `{"state": "on"}` は既存の `value.state` 経路で `classify_str` に届くため
  追加不要。casa ラップ `{"value": {...}}` も既存経路でそのまま通る。
- プロパティ探索は現状「`open_close_state` を優先、無ければ先頭」。switch の get_state は
  operation_status を指すので、そのプロパティが先頭に来る＝先頭 fallback で拾える。変更不要。
- 想定外の値・スキーマは従来どおり `State::Unknown`。

> 値の表現（`{"state":"on"}` か生 `0x30` か等）は機種・バックエンドで振れるため、
> 上記のように文字列/数値/オブジェクトを幅広く受ける（shutter と同じ寛容さ）。
> 実機での確定形は config 投入時に `enl describe` で確認する（config.example に注記）。

### 3. main.rs — ルーティング

- `fetch_state`: `match device.kind` に `Kind::Switch => normalize_enl_state(&raw)` を追加。
- `device_op`: 既存の `Some(cmd) if device.kind == Kind::Light => run_light_action(...)`（best-effort）は
  そのまま。switch はこの条件に**該当しない**ので `Some(cmd) => run_action(...)`（同期確認）に
  自然に落ちる。追加コード不要。
- `list_devices` の `DeviceInfo` は kind・stop・presets 等を既に汎用で載せている。switch は
  stop=false・presets=[]・color/brightness=false になるだけ。変更不要。

### 4. State enum

`State::On` / `State::Off` は light 用に既存。switch も同じ値を使う（追加不要）。

## フロントエンド設計（index.html）

switch は **light のタイル見た目**を借り、**振る舞いは shutter 系**（ポーリング＋同期確認）。
既存の kind 分岐が `kind === "light"` を特別扱いしているため、switch はその条件を
素通りして自動的に shutter 系の挙動（同期パス・ポーリング）に乗る。

### 追加/変更

1. **`buildSwitchTile(dev)`** を追加（`buildLightTile` を土台に簡素化）:
   - タイル DOM（`.tile` + アイコン + `.tname` + `.status`）。アイコンは電源記号（⏻）。
   - 色調整ボタン・シート連携は**持たない**（switch に色/明るさ/プリセットは無い）。
   - カード state は `{ kind: "switch", rootEl, statusEl, labelEl, msgEl, buttons, state }`。
   - アイコンの click で `deviceAct(dev.name, c.state === "on" ? "off" : "on")`。
     `deviceAct` は `kind !== "light"` なので**同期パス**（`run_action` 相当）を通る。

2. **状態ラベル**: light は「点灯/消灯」。switch は「オン/オフ」。
   `renderState` を kind 対応にし、`kind === "switch"` のとき switch 用ラベル
   （`{on:"オン", off:"オフ", unknown:"不明"}`）を使う。`.lit` クラスの点灯演出
   （`rootEl.classList.toggle("lit", st === "on")`）は switch も共用。

3. **レイアウト分岐**（`boot()` 末尾）:
   - 現状 `shutters = devices.filter(d => d.kind !== "light")` は switch を誤って
     shutter カードに巻き込む。これを **`d.kind === "shutter"`** に狭める。
   - `switches = devices.filter(d => d.kind === "switch")` を追加。
   - switch があれば独立セクション（見出し「🔌 スイッチ」など）にタイルグリッドで並べる。
   - light セクション・shutter セクションのロジックはそのまま。

4. **ポーリング / 初期取得**（変更不要、確認のみ）:
   - `pollOnce` は `kind === "light"` のみスキップ。switch は非 light なので**ポーリングされる**。
   - 初期 state は `refreshOnce()`（= pollOnce）が非 light を取得するので switch も拾う。
   - `fetchLightStatesOnce` は内部で `kind === "light"` のみ対象。switch は素通り（二重取得しない）。
   - `MOVING_STATES`（opening/closing）は switch に無いので延長ロジックは無害に不発。

## config.example.toml

`kind = "switch"` の例を追記（casa リード + enl 直の代替を併記、shutter/light の例に倣う）:

```toml
# ── ECHONET on/off 機器（換気扇・床暖房 on/off 等。enl/casa 経由）─────
# kind = "switch" は on / off / get_state 必須。色・明るさ・プリセット・stop は不可。
# get_state は operation_status（共通プロパティ EPC 0x80）を読む。値域は機種で振れる
# ため、投入前に `enl describe <IP> <EOJ>` で operation_status の返り形（on/off の表現）を確認する。
# set 後は mando が同期で state を取り直して確定表示する（shutter と同じ正直な確認）。
# [[device]]
# name  = "fan"
# alias = "換気扇"
# kind  = "switch"
# casa 経由（推奨）:
# get_state = ["casa", "get", "fan", "operation_status"]
# on        = ["casa", "set", "fan", "operation_status", "on"]
# off       = ["casa", "set", "fan", "operation_status", "off"]
# enl を直接叩く形（casa を挟まない場合）:
#   get_state = ["enl", "get", "192.0.2.20", "<EOJ>", "operation_status"]
#   on        = ["enl", "set", "192.0.2.20", "<EOJ>", "operation_status", "on"]
#   off       = ["enl", "set", "192.0.2.20", "<EOJ>", "operation_status", "off"]
#
# 注意: switch はグループ（[[group]] members）に入れられない（当面シャッター専用）。
```

## テスト

### config.rs（ユニット）
- switch: `on`/`off` 必須（欠けたら `MissingCommand`）。
- switch: `open`/`close`/`stop`/`color`/`brightness`/`preset` 指定は `ForbiddenField`。
- switch をグループメンバーにしたら `NonShutterInGroup`（light も同エラーになることを確認）。
- switch が正常にパースされ `kind == Kind::Switch`、`on_cmd`/`off_cmd` が取れる。

### normalize.rs（ユニット）
`normalize_enl_state` に対し:
- operation_status オブジェクト `{"properties":[{"epc":"80","name":"operation_status","value":{"state":"on"}}]}` → `On`、`off` → `Off`。
- 文字列値 `"on"`/`"off"` → `On`/`Off`。
- 数値値 `0x30`/`0x31` → `On`/`Off`。
- casa ラップ `{"value":{"properties":[...on...]}}` → `On`。
- 想定外値（例 `"heating"`）→ `Unknown`。
- 既存の開閉テスト（open/closed 等）が退行していないこと。

### 手動確認
- `cargo build --release` / `cargo test` / `cargo clippy -- -D warnings`。
- switch を含む config でサーバ起動し、タイルが「スイッチ」セクションに出る／タップで
  on↔off がトグルし set 後の同期確認で状態が確定する／アクティブ窓でポーリングされる、を確認。

## 変更ファイル一覧

| ファイル | 変更 |
|--|--|
| `src/config.rs` | `Kind::Switch`、検証アーム、グループ検証の一般化（`LightInGroup`→`NonShutterInGroup`） |
| `src/normalize.rs` | `normalize_enl_state` を on/off 対応に拡張、テスト追加 |
| `src/main.rs` | `fetch_state` に Switch アーム |
| `index.html` | `buildSwitchTile`、switch ラベル、レイアウト分岐、`renderState` の kind 対応 |
| `config.example.toml` | switch の例を追記 |

## 設計原則との整合

- **原則1（プロトコルを喋らない）**: switch も config のコマンド配列を exec するだけ。
- **原則2（バックエンド非依存）**: enl→casa は config 差し替えのみ。例に両形を併記。
- **原則3（フロント隔離）**: フロントは `kind` と on/off/unknown しか知らない。EPC も enl も知らない。
- **原則4（下層知識を一点に）**: on/off 正規化も `normalize_enl_state` の中だけ。casa 移行時もここだけ。
- **原則5（直列化）**: 既存 `Semaphore(1)` を通る（新規 exec 系統を作らないため自動的に従う）。
- **原則6（pull・アクティブ窓）**: switch はポーリング対象。窓の内側だけ取得。
- **原則7（正直な成否）**: switch は set 後に同期で state 再取得（light の例外ではなく shutter 側）。
