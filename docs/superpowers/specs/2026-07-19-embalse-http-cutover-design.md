# embalse-query 読み出しを ssh+CLI から NAS の HTTP API へ切替（config のみ）

- 対象 issue: [#1](https://github.com/nogu3/mando/issues/1)
- 日付: 2026-07-19
- 方式: **config のみ**。mando の Rust コードは変更しない。

## 背景

2026-07-19 に embalse の NAS カットオーバーが完了し、`embalse-query serve`（axum）が
NAS 上に常駐して契約 JSON を HTTP 提供する状態になった。

- ベース URL: `http://192.168.1.138:8526`
- `GET /api/graphs/{name}?period={period}` — グラフ契約 JSON（series 配列）
- `GET /api/health` — マシン健全性（常に 4 要素・raw 未作成でも 200）
- 返る JSON は従来の CLI 出力と同一（同じ serve が同じ duckdb クエリをラップ）

これにより mando は ssh 経由で jarvis の `embalse-query` CLI を exec する必要が無くなる。

## 現状（mando 側）

- グラフ: `get_graph`（`src/main.rs`）が config の `[[graph]].query` テンプレの `{period}`
  を検証済み値（today/week/month）に置換し、`graph_executor`（`Semaphore(1)`）で subprocess
  exec。stdout を契約 JSON 配列としてパース → `normalize_graph_rows` で系列化。
- health: `get_health` が config の `[health].command` を同様に exec → `normalize_health_rows`。
- 両ハンドラは exec の `outcome != ExecOutcome::Success` かどうかだけを見る。非成功は一律
  `graph_unavailable` / `health_unavailable`（**502**）。成功時は stdout の JSON 配列のみ使用。
- exec 経路（`src/exec.rs` の `Executor`）は「コマンド配列を exec して stdout/stderr/exit を返す」
  だけの汎用抽象。コマンドの中身が何かは知らない。

## 決定: curl を config テンプレに（コード変更ゼロ）

`embalse-query` テンプレを curl テンプレに差し替えるだけで切替が完了する。

```toml
# graph
query   = ["curl", "-fsS", "--max-time", "30",
           "http://192.168.1.138:8526/api/graphs/generation?period={period}"]
# health
command = ["curl", "-fsS", "--max-time", "30",
           "http://192.168.1.138:8526/api/health"]
```

### なぜ curl（native HTTP クライアントではなく）か

- **原則2（バックエンド非依存）に完全一致。** mando 本体は「不透明なコマンドを exec して
  契約 JSON を受け取るサーバ」のまま。HTTP を喋るのは curl であって mando ではない。
- **原則1（transport を持ち込まない）を維持。** UDP/3610 を持ち込まないのと同じ精神で、
  HTTP クライアントも mando 本体に持ち込まない。transport は config 側（curl）に閉じる。
- **ゼロコード・ゼロ新依存。** reqwest 等を足さない。テストも増えない。
- curl は jarvis（Raspberry Pi）で確実に使える。

### curl フラグ

`curl -fsS --max-time 30 <URL>`

| フラグ | 意図 |
|---|---|
| `-f` | HTTP ≥400 で非ゼロ終了＋stdout 空 → 既存の「非 Success は 502」に落ちて正直に失敗 |
| `-s` | プログレスメータ抑制 |
| `-S` | エラーだけは stderr に出す（mando が失敗時に stderr をログ → 診断に効く） |
| `--max-time 30` | 契約の 30s 上限と一致。`GRAPH_QUERY_TIMEOUT`（tokio timeout）と二重で守る |

### URL の形

- グラフ: `http://192.168.1.138:8526/api/graphs/{graph名}?period={period}`
  - `{graph名}`（generation/co2/machine…）は各エントリ固定で URL に直書き
  - `{period}` は mando が today/week/month に検証置換（現行踏襲）
- health: `http://192.168.1.138:8526/api/health`（period なし）

### ベース URL の config 化

ベース URL はコードにハードコードせず config（curl テンプレの URL 文字列）に置く。
graph×N + health の各エントリに**繰り返し**書く形になるが、これは「コードのハードコード」
ではなく「config の重複」であり、原則的にセーフ（mando 本体を触らないことを優先）。

## 既知の割り切り（コスメティック）

curl の非ゼロ exit code（例: 7=connect refused, 28=timeout）は `ExecOutcome::from_code`
が enl 用マッピング（3=timeout/4=rejected/5=network）で解釈するため、**ログの `outcome`
ラベルが実態とズレる**ことがある。ただしグラフ/health ハンドラは `outcome != Success` しか
分岐に使わないので**挙動は正しく一律 502**。ラベルのズレはログのみ。mando 本体を触らない
方針を優先し、コードでは直さない。

## 変更対象

1. `config.example.toml`
   - グラフ/health のコメントブロックと例を curl ベースへ更新。
   - framing: 「読み出し CLI をテンプレで指定」→「下層の読み出しコマンドをテンプレで指定。
     現状は NAS の HTTP API を curl で叩く。mando は transport（HTTP か CLI か）を知らない」。
2. jarvis の実 config（`/etc/mando/config.toml`）
   - `embalse-query` テンプレを curl テンプレへ差し替え、mando サービスを再起動。

## 完了条件

- jarvis 上の mando で `/api/graphs/{全種}`（各 period）と `/api/health` を叩き、契約 JSON が
  従来と一致することを実機確認。
- ssh + jarvis CLI（`/usr/local/bin/embalse-query`）への依存が mando 側から消える。
- soak 後、jarvis 側の `/usr/local/bin/embalse-query` は embalse 側で撤去予定（mando が HTTP を
  叩けることが前提）。本 issue のスコープは mando 側切替まで。

## 非スコープ

- mando の Rust コード変更。
- ベース URL を 1 箇所に集約する config スキーマ変更（native HTTP 経路 = 案 B）。
- jarvis 側 `embalse-query` バイナリの撤去（embalse 側の責務）。
