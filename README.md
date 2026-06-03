# mando

スマートホーム操作の **Web フロント**。スマホから家電（電動シャッター等）を操作する常駐 HTTP サービス。

プロトコルは喋らない。`casa`（ブートストラップ期は `enl`）を subprocess で呼ぶだけ。
成果物は **バイナリ 1 個 + `config.toml`**（`index.html` は焼き込み）。

設計の詳細は [CLAUDE.md](./CLAUDE.md) を参照。

## クイックスタート

```bash
cp config.example.toml config.toml   # 実 IP・EPC を埋める（リポジトリには含めない）
cargo build --release                # → 単一バイナリ
MANDO_CONFIG=./config.toml ./target/release/mando
```

スマホから `http://<ホストの LAN IP>:8080/` を開く。

## API（安定ミニ API）

- `GET  /` — 焼き込んだ UI
- `GET  /api/devices` — 論理デバイス一覧
- `GET  /api/devices/{name}/state` — `{ state: "open|closed|unknown", exec, raw }`
- `POST /api/devices/{name}/open` — open → **直後に state 再取得** → `{ action, state, exec, raw }`
- `POST /api/devices/{name}/close` — 同上（close）

`state` は set 後に必ず取り直した確定値（楽観表示しない）。`action` / `exec` は
subprocess の終了コードを写したもの: `success` / `timeout` / `rejected` /
`network_error` / `failed` / `spawn_failed`。

## 設定

`MANDO_CONFIG` 環境変数で `config.toml` の場所を指定（既定 `./config.toml`）。
形式は [`config.example.toml`](./config.example.toml) を参照。論理デバイスごとに
`get_state` / `open` / `close` のコマンド配列を持ち、本体はそれをそのまま exec する。

## ラズパイ常駐（systemd, 推奨）

Pi 上で常駐させる正攻法。自動起動・異常時再起動・ログを systemd に任せる
（mando 自身は前景プロセスのまま。self-fork での daemon 化はしない）。

```bash
# Pi 上で（要 git/cargo。enl も Pi の PATH に置くか config を絶対パスに）
task install        # ビルド → /usr/local/bin/mando、unit 配置、enable --now
sudo nano /etc/mando/config.toml   # 実 IP・EPC を編集
task reload    # 反映
task logs           # ログ追尾（journalctl -u mando -f）
```

- バイナリ: `/usr/local/bin/mando`、config: `/etc/mando/config.toml`、
  unit: `/etc/systemd/system/mando.service`（テンプレは `deploy/mando.service`）
- ポートは mando=8080 / enl=3610 とも >1024 → 一般ユーザで可（root 不要）
- enl/casa が UDP・マルチキャストでデバイスと話すため、Pi とデバイスは同一 LAN に
- 削除: `task uninstall`

## デプロイ（Docker, ホストネットワーク必須）

```bash
cp config.example.toml config.toml   # 実値を埋める
docker compose up -d
```

`mando` を動かすホストと対象デバイスは**同一 L2 / VLAN** にあること。ブリッジ
ネットワークでは UDP のデバイス応答とマルチキャストを受けられないため
`network_mode: host` が必須。ポート 3610 は `enl` / `casa` が使う。

> 注: `enl` / `casa` のバイナリはこのコンテナに別途同梱する必要がある（Dockerfile のコメント参照）。

## 開発

```bash
cargo test
cargo clippy -- -D warnings
RUST_LOG=mando=debug cargo run
```
