# embalse-query HTTP 切替 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** mando の embalse-query 読み出しを ssh+CLI から NAS の HTTP API（`curl`）へ切り替える。mando の Rust コードは変更しない。

**Architecture:** config の graph/health テンプレを `["embalse-query", ...]` から `["curl", "-fsS", "--max-time", "30", "<NAS URL>"]` へ差し替えるだけ。mando 本体は「不透明なコマンドを exec して契約 JSON 配列を受け取るサーバ」のままで、transport（HTTP か CLI か）を知らない。`get_graph` / `get_health` / `Executor` は無変更。

**Tech Stack:** TOML config、`curl`、既存の axum サーバ（無変更）、jarvis（Raspberry Pi）デプロイ。

## Global Constraints

- mando の Rust コード（`src/**`）は**一切変更しない**。変更は config のみ。
- curl フラグは `-fsS --max-time 30` 固定（`-f`=HTTP≥400 で非ゼロ終了→502、`-s`=進捗抑制、`-S`=エラーは stderr、`--max-time 30`=契約 30s 上限）。
- NAS ベース URL: `http://192.168.1.138:8526`。グラフは `/api/graphs/{graph名}?period={period}`、health は `/api/health`。
- `{period}` プレースホルダは現行踏襲（mando が today/week/month に検証置換）。`{graph名}` は各エントリの URL に直書き。
- Spec: `docs/superpowers/specs/2026-07-19-embalse-http-cutover-design.md`。

---

### Task 1: config.example.toml の graph/health を curl ベースへ更新

**Files:**
- Modify: `config.example.toml:131-173`（グラフ節と health 節のコメント＋例）

**Interfaces:**
- Consumes: なし（純ドキュメント。すべて `#` コメントアウトされた例で、パースされない）
- Produces: なし（後続タスクは jarvis 実 config を独自に編集する。本タスクは形の参照元）

- [ ] **Step 1: グラフ節（131-160 行）のコメントと例を差し替え**

現在の 131-160 行:
```toml
# ── グラフ（きろくセクション）──────────────────────────────
# embalse 等の読み出し CLI をテンプレで指定する。{period} は today / week /
# month（検証済みの 3 値のみ）に置換して exec される。CLI は stdout に
#   [{"ts":"2026-07-15T10:00:00+09:00","series":"書斎","value":812.0}, ...]
# の JSON 配列を返すこと（series は単系列なら省略可）。SQL・スキーマ・
# データパスは embalse 側の責務で、mando はコマンド名すら知らない。
#
# [[graph]]
# name       = "generation"
# label      = "太陽光発電"
# unit       = "W"      # 今日ビュー（時系列カーブ）の単位
# unit_daily = "kWh"    # 週/月ビュー（日別集計）の単位。省略時 unit
# query      = ["embalse-query", "generation", "{period}"]
#
# [[graph]]
# name  = "co2"
# label = "CO2"
# unit  = "ppm"
# query = ["embalse-query", "co2", "{period}"]
#
# [[graph]]
# name  = "machine"
# label = "jarvis"
# unit  = "%"      # 温度だけ series_labels 側で ℃ を明示
# query = ["embalse-query", "machine", "{period}"]
# [graph.series_labels]  # series 名 → UI 表示名（任意。無いキーは素通し）
# cpu_used_pct  = "CPU (%)"
# mem_used_pct  = "メモリ (%)"
# disk_used_pct = "ディスク (%)"
# cpu_temp_c    = "温度 (℃)"
```

これを次に置き換える:
```toml
# ── グラフ（きろくセクション）──────────────────────────────
# 下層の読み出しコマンドをテンプレで指定する。mando はこのコマンドを exec し
# stdout の契約 JSON 配列を受け取るだけで、transport（HTTP か CLI か）は知らない。
# {period} は today / week / month（検証済みの 3 値のみ）に置換される。
# コマンドは stdout に
#   [{"ts":"2026-07-15T10:00:00+09:00","series":"書斎","value":812.0}, ...]
# の JSON 配列を返すこと（series は単系列なら省略可）。SQL・スキーマ・データパスは
# embalse 側の責務。現状は NAS 常駐の embalse-query serve（:8526）を curl で叩く:
#   curl -fsS --max-time 30 <URL>
#   -f=HTTP≥400 で失敗（→ mando は 502）／-s=進捗抑制／-S=エラーは stderr へ
#   --max-time 30=契約の 30s 上限。ベース URL は下の各例に直書き（設置環境で書換）。
#
# [[graph]]
# name       = "generation"
# label      = "太陽光発電"
# unit       = "W"      # 今日ビュー（時系列カーブ）の単位
# unit_daily = "kWh"    # 週/月ビュー（日別集計）の単位。省略時 unit
# query      = ["curl", "-fsS", "--max-time", "30", "http://192.0.2.20:8526/api/graphs/generation?period={period}"]
#
# [[graph]]
# name  = "co2"
# label = "CO2"
# unit  = "ppm"
# query = ["curl", "-fsS", "--max-time", "30", "http://192.0.2.20:8526/api/graphs/co2?period={period}"]
#
# [[graph]]
# name  = "machine"
# label = "jarvis"
# unit  = "%"      # 温度だけ series_labels 側で ℃ を明示
# query = ["curl", "-fsS", "--max-time", "30", "http://192.0.2.20:8526/api/graphs/machine?period={period}"]
# [graph.series_labels]  # series 名 → UI 表示名（任意。無いキーは素通し）
# cpu_used_pct  = "CPU (%)"
# mem_used_pct  = "メモリ (%)"
# disk_used_pct = "ディスク (%)"
# cpu_temp_c    = "温度 (℃)"
```

> IP は例なので RFC 5737 のドキュメント用レンジ（`192.0.2.20`）を使う。実 NAS IP は
> 各自ローカルで置換（jarvis の実値は `192.168.1.138`）。

- [ ] **Step 2: health 節（162-173 行）のコメントと例を差し替え**

現在の 162-173 行:
```toml
# ── マシン健全性（任意）─────────────────────────────
# 下層の health CLI（embalse-query health 等）を exec し、異常時のみ画面上部に
# バナーを出す。未設定ならこの機能ごと無効。しきい値判定は下層の責務。
#
# [health]
# label   = "jarvis"                       # バナーの対象名（任意）
# command = ["embalse-query", "health"]
# [health.labels]  # metric 名 → 表示名（任意。無いキーは素通し）
# cpu_used_pct  = "CPU"
# mem_used_pct  = "メモリ"
# disk_used_pct = "ディスク"
# cpu_temp_c    = "CPU温度"
```

これを次に置き換える:
```toml
# ── マシン健全性（任意）─────────────────────────────
# 下層の health 読み出しコマンドを exec し、異常時のみ画面上部にバナーを出す。
# 未設定ならこの機能ごと無効。しきい値判定は下層の責務。グラフ同様、現状は NAS 常駐の
# embalse-query serve（:8526）を curl で叩く（period なし）。
#
# [health]
# label   = "jarvis"                       # バナーの対象名（任意）
# command = ["curl", "-fsS", "--max-time", "30", "http://192.0.2.20:8526/api/health"]
# [health.labels]  # metric 名 → 表示名（任意。無いキーは素通し）
# cpu_used_pct  = "CPU"
# mem_used_pct  = "メモリ"
# disk_used_pct = "ディスク"
# cpu_temp_c    = "CPU温度"
```

- [ ] **Step 3: 回帰がないことを確認**

Run: `cargo test`
Expected: 全テスト PASS（config.example.toml はどのテストにもパースされないため、
コメント変更で挙動は変わらない。緑のままであることの確認）。

- [ ] **Step 4: コメントアウトの一貫性を目視確認**

Run: `grep -n '^[^#]' config.example.toml | grep -iE 'graph|health|curl|embalse'`
Expected: 出力なし（グラフ/health 節が誤って uncomment されていない＝すべて `#` 始まり）。

- [ ] **Step 5: Commit**

```bash
git add config.example.toml
git commit -m "docs: config.example の graph/health 例を curl(HTTP) ベースへ更新

embalse-query の NAS カットオーバー（HTTP :8526）に合わせ、読み出しテンプレを
curl -fsS --max-time 30 <URL> に差し替え。mando は transport 非依存のまま。
refs #1

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01KdELFYS55V645tc9UsweTo"
```

---

### Task 2: jarvis の実 config を差し替え、mando 再起動、実機検証

**Files:**
- Modify: jarvis 上 `/etc/mando/config.toml`（root 所有。graph の `query` と health の `command`）

**Interfaces:**
- Consumes: Task 1 で確定した curl テンプレの形（フラグ順・URL 形）。実 IP は `192.168.1.138`。
- Produces: HTTP 経由で全グラフ + health が契約 JSON を返す実機状態。

> **運用スキル:** 実機操作は `jarvis` skill（サービス状態・restart・ログ）と `despliegue` skill
> （配布手順）を参照。config-only の差し替えなのでバイナリ再ビルドは不要。

- [ ] **Step 1: jarvis の現行 mando config を確認**

jarvis skill 経由で `/etc/mando/config.toml` を読み、現在の `[[graph]]` 全種（generation/co2/machine 等）と `[health]` の `query`/`command` が `embalse-query` を使っていることを確認。実在するグラフ名の一覧を控える（Step 4 の検証対象）。

- [ ] **Step 2: config をバックアップしてから curl テンプレへ差し替え**

`/etc/mando/config.toml` は root 所有。既存のバックアップ運用（home 配下、[[jarvis-mando-deploy-path]]）に従い現行を退避してから、各 `[[graph]].query` を
```toml
query = ["curl", "-fsS", "--max-time", "30", "http://192.168.1.138:8526/api/graphs/<name>?period={period}"]
```
に、`[health].command` を
```toml
command = ["curl", "-fsS", "--max-time", "30", "http://192.168.1.138:8526/api/health"]
```
に差し替える。`<name>` は各グラフの `name`（generation/co2/machine…）。それ以外の
フィールド（label/unit/series_labels/labels 等）は触らない。

- [ ] **Step 3: mando サービスを再起動**

jarvis skill 経由で mando サービスを restart し、起動ログにエラー（config パース失敗等）が無いことを確認。

- [ ] **Step 4: NAS 到達性と mando 経由の契約 JSON を実機検証**

jarvis 上から順に確認する:

1. NAS 直叩き（transport の健全性）:
   `curl -fsS --max-time 30 'http://192.168.1.138:8526/api/health'` が JSON 配列を返す。
2. mando 経由（切替の本体。全グラフ × 各 period）:
   - `curl -fsS 'http://<mando>/api/graphs/<name>?period=today'`（week/month も）
   - Step 1 で控えた全グラフ名について実施。各々 `{"name":..,"series":[...]}` が返る。
3. mando 経由 health:
   - `curl -fsS 'http://<mando>/api/health'` が `{"worst":..,"items":[...]}` を返す。

Expected: 全て 200 + 契約 JSON。従来（CLI 経由）と同じ系列・値域であること。502 が出たら
mando ログの stderr（curl のエラー）で切り分ける。

- [ ] **Step 5: ブラウザ実機確認（きろくセクション）**

スマホ or ブラウザで mando の UI を開き、きろくセクションの各グラフが描画され、health
バナーが（異常時のみ）機能することを目視確認。ssh + jarvis CLI に触れず HTTP のみで
成立していることを確認（完了条件）。

- [ ] **Step 6: 完了記録**

- issue #1 に「mando 側 HTTP 切替完了・実機検証済み」を記録（jarvis 側 `embalse-query`
  バイナリ撤去は embalse 側スコープなので issue はまだ close しない／その旨コメント）。
- メモリ [[embalse-query-cli-pending]] と [[jarvis-mando-deploy-path]] を現状（HTTP 切替済み）
  に更新。

---

## Self-Review

- **Spec coverage:** curl フラグ（Global Constraints + Task1）・URL 形（両タスク）・base URL の config 化＝各エントリ直書き（Task1 コメント）・exit code の割り切り（spec に記載、コード無変更なので plan には作業なし）・config.example 更新（Task1）・jarvis 実 config 差し替え＋検証（Task2）・完了条件（Task2 Step4-5）を全てカバー。
- **Placeholder scan:** TBD/TODO なし。編集は現行行を丸ごと提示し置換後を全文提示。Task2 の `<name>`/`<mando>` は「実環境で確定する値」であり未定義プレースホルダではない（Step1 で控える手順を明記）。
- **Type consistency:** コード変更なし。curl フラグ順（`-fsS --max-time 30`）と URL パス（`/api/graphs/{name}`・`/api/health`）は全所で一致。
