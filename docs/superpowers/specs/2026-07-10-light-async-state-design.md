# light の操作と状態取得の分離（非同期確認）設計

日付: 2026-07-10
状態: 承認済み（実装は未着手 — 次セッションで writing-plans → Subagent-Driven で実施）

## 背景 / 動機

light（mat wire group の groupcast）は、操作 POST が「コマンド送信 + 確認の state 読み」を
**同期**で行うため、ボタンが ~2.3 秒塞がる。利用者の優先度は
「state の正確さ < ボタンをすぐ押し直せること」（ユーザー明言）。
groupcast は無応答マルチキャストで、確認読み自体が代表ノード 1 台のプロキシ読みに
すぎず、確認としてもともと弱い。

なお応答性の下回りは前セッションで対処済み（jarvis: matd を systemd 化・
`--idle-timeout 86400`・固定 socket `/run/matd/matd.sock`、mando unit に drop-in で
`MAT_MATD_SOCKET` 設定済み）。本設計はその上に載る UI/API の分離。

## 決定事項

1. **light の `POST /api/devices/{name}/on|off|presets/{preset}` は exec 結果のみ返す。**
   state の再取得・同梱をやめる。レスポンス形は `{"action": "<ExecOutcome>"}` のみ。
2. **UI は押下後に裏で state を 1 回だけ追いつき取得**（目安: 2 秒後）。
   ボタンは POST 応答（groupcast ~1 秒）で即解放し、追いつき取得中も塞がない。
   追いつき取得時に当該デバイスが busy（連打中）なら取得をスキップ。
   取得結果は renderState で反映（ベストエフォート表示であることを受け入れる）。
3. **shutter は現行どおり変更しない。** 設計原則 7（set 後の同期確認）は
   シャッターの「確実に開閉」のためのものなので維持。分岐は `kind` で行う。
4. **設計原則 7 の適用範囲を明文化する。** CLAUDE.md の原則 7 に
   「light は例外: 操作は送信結果のみ正直に返し、state は非同期のベストエフォート表示」
   の注記を追加。`docs/superpowers/specs/2026-07-09-light-device-design.md` は
   歴史文書としてそのまま（本書が上書きする）。
5. **jarvis の実 config の `sleep 0.5` ラッパーを撤去**し素の `mat read` に戻す
   （同期確認がなくなるので競合回避の待ちは不要。UI 側の 2 秒遅延が同じ役を担う）。
   `config.example.toml` の該当コメント（sh ラッパーの型）も本設計に合わせて更新。

## API（変更点）

- light への `POST .../on|off|presets/{preset}` → `200 {"action": "success" | "timeout" | ...}`
  （`state` / `exec` / `raw` は含めない）
- shutter への POST・`GET .../state`・その他エンドポイントは無変更。
- 404 系（kind 不整合・未知 preset・未知 device）は無変更。

## 実装ポイント

- `src/main.rs`:
  - `device_op` / `preset_device` で `device.kind == Kind::Light` なら
    `run_action`（exec + fetch_state）でなく exec のみ実行し、
    `{"action": outcome}` を返す（新しい小さな Serialize 構造体 `LightActionView` 等）。
  - 既存テストの修正: `light_on_returns_confirmed_state` / `preset_runs_and_confirms_state`
    は「`action` のみ返り `state` キーが無い」ことの検証に変える。
    `GET state` 系・shutter 系テストは不変のまま通ること。
- `index.html`:
  - light の `deviceAct` / `presetAct`: POST 応答で即 `setDeviceBusy(false)`。
    応答の `action` が非 success ならエラーメッセージ表示（現行の ACTION_MSG 流用）。
    成功時は状態ラベルを「反映中…」等の中間表示にし、`setTimeout` ~2000ms 後に
    `GET state` → renderState（busy 中ならスキップ。タイマーは device ごとに 1 本、
    連打時は張り直し）。
  - shutter の経路・ポーリングは触らない。
- デプロイ: `task deploy`（クロスビルド → scp → 再起動）。
  jarvis の `/etc/mando/config.toml` から get_state の sh ラッパーを素に戻す
  （要 ssh + sudo。ユーザー承認は本設計で得ている）。

## テスト / 検証

- cargo test: 上記テスト修正込みで全通過、clippy クリーン。
- e2e（jarvis）: `time curl -X POST .../on` が ~1 秒前後で `{"action":"success"}` を返す。
  数秒後の `GET .../state` が実状態に追いつくこと。off で復元して終える。
- UI 実機: 押下 → 1 秒前後でボタン再有効、状態ドットが数秒遅れで追いつくこと。

## やらないこと

- shutter の非同期化（原則 7 維持）。
- state 追いつき取得の複数回リトライや購読的なポーリング復活（1 回でよい。
  外れていても次の操作 or ページ再表示で直る）。
- API バージョニング（利用者は同梱 UI のみ。ActionView 形の互換性維持は不要）。
