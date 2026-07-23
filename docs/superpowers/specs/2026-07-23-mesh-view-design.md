# メッシュ表示（`mat diag mesh` の可視化）設計

- 日付: 2026-07-23
- 関連: [mat#12](https://github.com/nogu3/mat/issues/12)（mat 側 = JSON 出力。実装済み・mat 1.1.0）
- 対象: mando に `/mesh` 画面と `[mesh]` config を追加し、Thread メッシュのトポロジーとリンク品質を表示する

---

## 目的

Thread メッシュの健康度を継続的に見られるようにする。「どこが弱いか」「どこ↔どこが繋がっているか」が一目で分かること。利用者は**技術者本人**であって家族ではない。

## 前提となる実測（2026-07-23、設置環境）

設計はこの実測に強く依存するので記録する。

| 観測 | 値 |
|---|---|
| commission 済みノード数 | 13 |
| `mat diag mesh` 所要時間 | **1 分 45 秒**（1 台が mDNS timeout で失敗し、それ単体で数十秒） |
| 出力頂点数 / エッジ数 | 27 / 134 |
| neighbor の `lqi` スケール | **0–3**（README 例の 0–255 ではない） |
| `avg_rssi` の範囲 | -55 〜 -102 dBm |
| 自己同定に失敗した自 fabric ノード | 13 台中 8 台 |

`frame_error_rate` は LQI と独立に振れる。LQI 1・誤り率 0% のリンクが多数ある一方、LQI 1・誤り率 88% のリンクも存在する。**LQI だけでは弱いリンクを取りこぼす。**

自己同定の失敗は mat の既知問題（ESP32 系デバイスが同一の firmware ハードコード HardwareAddress を名乗るため、mat が全員の自己同定を無効化する）。結果として同じ無線機が `node:<id>`（自分の視点）と無名の `ext:<hex>`（他ノードから見た姿）の 2 頂点に分裂する。**mando はこれを推測で統合しない**（原則 4: 下層の課題を上層で埋めない）。mat 側にフォローアップ issue を立てる。

---

## 全体の形

```
ブラウザ  ──GET /mesh──────────────▶  mesh.html（include_str! で焼き込み）
          ──GET /api/mesh─────────▶  スナップショット + ジョブ状態（即答）
          ──POST /api/mesh/refresh▶  202（ジョブ起動、single-flight）

mando ────exec────▶  mat diag mesh  ──▶  生 JSON
      ────normalize::normalize_mesh──▶  MeshView（安定形）
```

## 1. config — `[mesh]`（任意）

未設定なら `/mesh` と `/api/mesh` は 404。既存の `[health]` と同型。

```toml
[mesh]
label      = "自宅メッシュ"          # 任意。未指定なら名前なし
command    = ["mat", "diag", "mesh"]
ttl_ms     = 600000                 # 既定 600000（10 分）。これより古ければ開いた時に自動再取得
timeout_ms = 240000                 # 既定 240000（4 分）。graph の 30s とは別枠

# 「弱い」の判定。実測に合わせて 2 軸で持つ
[mesh.thresholds]
lqi_fair = 2      # 既定 2。これ以下は fair
lqi_weak = 1      # 既定 1。これ以下は weak
fer_weak = 30     # 既定 30。frame_error_rate(%) がこれ以上なら LQI に関わらず weak

# alias → 表示名の置換（任意）。[health.labels] と同型。無いキーは alias を素通し
[mesh.labels]
living_south_light = "リビング南"
desk_light         = "デスクライト"
```

`command` が空配列なら config エラー（`[health]` と同じ扱い）。

## 2. API

| endpoint | 応答 | 備考 |
|---|---|---|
| `GET /api/mesh` | 200 即答 | `{ status, age_ms?, elapsed_ms?, snapshot?, error? }` |
| `POST /api/mesh/refresh` | 202 即答 | 既に `running` なら何もせず 202（single-flight） |

`status`:

| 値 | 意味 | UI |
|---|---|---|
| `empty` | まだ一度も取得していない | 「まだ調べていません」＋更新ボタン |
| `running` | 取得中 | 経過秒を表示。前回スナップがあれば薄く残す |
| `idle` | スナップショットあり・待機中 | 通常表示 |
| `failed` | 直近の取得が失敗 | 「調べられませんでした」＋理由 |

ジョブ状態は**メモリ内に 1 スロットだけ**保持する。永続化しない（mando 再起動で `empty` に戻る）。

exec は既存の `Executor` にレーン `"mesh"` を切って直列化する。ただし `mat diag mesh` は matd を経由しない直経路なので、照明操作（matd 経由）とは別レーンとなり同時に走りうる。これはレーン分離では解けないので、**取得中は照明操作が遅くなることがある**と受け入れる（UI にも書かない — 実害が出たら再検討する）。

> **原則との整合**: mando は「スケジューリング・自動化を持たない」（CLAUDE.md やらないこと）。ここで持つのは*ジョブ 1 本ぶんの進行状態とスナップショット*だけで、スケジューラは持たない。誰も `/mesh` を開いていなければ何も走らない。原則 6 の「見ている間だけ pull」を、2 分かかる pull に合わせて非同期化しただけと位置づける。

## 3. 正規化 — `normalize::normalize_mesh`

mat 固有の知識はこの関数だけに閉じ込める（原則 4）。フロントは `grade` を見るだけで、判定ロジックも生 JSON も知らない。

```rust
MeshView {
    network: { name, channel, leader_router_id, split: bool },  // split = partition_ids.len() > 1
    nodes: [{
        id,                 // mat の安定キーをそのまま（ext:… / node:… / rloc:…）
        name,               // labels 置換後の表示名。無名なら ext の先頭 4 桁
        kind,               // "ours"(alias あり) | "named"(label あり) | "anonymous"
        role,               // leader | router | reed | sed | unknown
        is_leader, is_border_router,
        probed,             // false なら error_kind を持つ
        error_kind,         // "timeout" | "unreachable" | "device_rejected" | …
    }],
    edges: [{
        a, b,
        grade,              // "good" | "fair" | "weak" | "bad" | "route_only"
        lqi, rssi, fer,     // 弱いリンク表の表示用。null あり
    }],
    fetched_at,
    unidentified_count,     // 自己同定に失敗した自 fabric ノード数（注記の出し分け用）
}
```

**grade の決め方** — `a_sees_b` / `b_sees_a` のうち**悪いほう**を採る（弱点探しが目的なので楽観側を採らない）。

1. 両方 null（route 情報のみ）→ `route_only`
2. `fer >= fer_weak` → `weak`
3. `lqi <= lqi_weak` → `lqi == 0` なら `bad`、それ以外は `weak`
4. `lqi <= lqi_fair` → `fair`
5. それ以外 → `good`

「悪いほう」は LQI で比較し、同値なら FER が大きいほうを採る。

## 4. 画面 — `/mesh`

`mesh.html` を `include_str!` でもう 1 枚焼き込む。成果物は引き続き**バイナリ 1 個 + config.toml**。トップの `index.html` からはリンクしない（家族の画面に技術者向けの重い操作を出さない）。

### 図（役割同心円）

- **中心** = leader
- **内リング** = router / reed
- **外リング** = child(sed)・役割不明・probe 失敗
- リング上の並び順は隣接ノードの平均角へ寄せる barycenter 反復（30 回）で交差を減らす
- **うちの機器** = 大きい丸＋名前、**otbr-br 等の既知の他人** = ◆、**leader** = ★、**無名の参加者** = 小さい灰点
- 線の色と太さ = `grade`（good 緑 / fair 黄 / weak 赤 / bad 濃赤・点線 / route_only 灰・点線）
- 無名ノードに触れる線は薄く描く
- 弱い線を後（＝上）に描き、良い線に埋もれさせない
- 「名無しを隠す」トグル（既定 OFF。実測では内リングの大半が無名ノードなので、隠すと図の骨格が失われる）

### 取得中の見せ方

- 開いた時に `age_ms > ttl_ms`（または `empty`）なら自動で `POST /api/mesh/refresh`
- `running` の間は 5 秒間隔で `GET /api/mesh` をポーリング
- 「13 台に順に問い合わせています… 経過 47 秒（ふつう 2 分ほど）」と**経過秒**を出す。mat は進捗を報告しないので**偽の進捗バーは出さない**
- 前回スナップショットがあれば薄く表示したまま、上に「更新中」を重ねる
- 「更新」ボタンを常設（`ttl_ms` を待たず強制再取得）

### 下部

- **ネットワーク情報ヘッダ**: 名前・チャンネル・機器数/参加者数・分裂の有無（`split` なら警告）
- **弱いリンク一覧**: `grade` が weak / bad のものを悪い順に。上位 10 件＋「他 N 件を表示」
- **問い合わせに失敗した機器**: 名前＋理由（応答なし / 届かない / 機器が拒否）
- **二重出現の注記**: `unidentified_count > 0` のとき常設。「うちの機器 N 台は自分の識別子を名乗れませんでした。そのぶん同じ機器が『名無しの参加者』としても図に出ている可能性があります」

### エラー時

`status: failed` なら「メッシュを調べられませんでした」＋ exit code 由来の理由を出す。ゼロ埋め・前回値のこっそり表示はしない（原則 7）。

## 5. テスト

**`normalize`**
- grade 判定: LQI 単独 / FER 単独 / 両方悪い / 片視点のみ / 両視点 null（route_only）
- 「悪いほう」採用: LQI 同値で FER 差がある場合
- `node:` 頂点と `ext:` 頂点の混在
- probe 失敗ノード（`probed: false` + `probe_error`）
- 空グラフ（`nodes: []`, `edges: []`）
- 閾値を config から変えたときに境界が動くこと

**`config`**
- `[mesh]` 有無 / 空 command の拒否 / `ttl_ms`・`timeout_ms`・thresholds の既定値 / `labels` の置換

**API**
- `GET /api/mesh` の 4 状態
- `POST /api/mesh/refresh` の single-flight（連打で 1 本しか走らない）
- `[mesh]` 未設定時に `/mesh` と `/api/mesh` が 404

## 6. やらないこと

- 自己同定に失敗したノードの推測統合（mat の責務）
- 定期実行・バックグラウンドポーリング（誰も見ていなければ走らせない）
- スナップショットの永続化・履歴（経時比較は将来の別テーマ）
- `--nodes` によるノード絞り込み（どのノードを選ぶかの知識が config に入るため）

## 7. 併せて立てる issue

- **mando 側 companion issue** — 本設計の実装（mat#12 が「mando repo に companion issue が要るかも」と書いているもの）
- **mat 側フォローアップ** — 同一 HardwareAddress を名乗るデバイス群の自己同定手段。解消すれば頂点の二重出現が消える
