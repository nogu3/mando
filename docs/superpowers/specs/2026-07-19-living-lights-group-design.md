# リビング照明のグループカード化(members 方式)— 設計

**日付:** 2026-07-19
**ステータス:** 承認済み

## 背景 / 目的

mat の個別ノード 9 台を mando に追加した結果、照明一覧にリビング系タイル
(リビング南・西・北、テレビ裏、下がり天井、あかり)が 6 枚並び、既存の
`living_lights`(mat wire group、multicast 一括操作)タイルと重複して雑然と
している。シャッターの「全部」カード(グループカード+「個別に操作 ▾」展開)
と同じ UX で、リビング照明を 1 枚のカードにまとめる。

前提知識: living_lights wire group(GroupId 10)の実メンバーは上記 6 台
(ユーザー確認済み。node 5, 7, 8, 10, 11, 14)。

## 方針(案 A: members 方式)

既存の `living_lights` デバイスがそのままグループカードになる。config で
親デバイスに `members` を持たせ、UI が「カード+展開」に描画する。
一括操作は既存の wire group コマンド(1 発 multicast)を変えない。

シャッター用 `[[group]]`(members を直列 exec)は流用しない — 一括 on/off が
6 台直列 exec になり multicast より明確に遅いため。

## config(`src/config.rs`)

- `Device` に `members: Vec<String>`(serde default 空)を追加。
- 起動時検証(既存 `ConfigError` 流儀):
  - members の各名前が存在するデバイスを指すこと
  - 親・メンバーとも `kind = "light"` であること
  - メンバー自身が members を持てない(入れ子禁止)
  - 同一デバイスが複数の親の member になれない
- `config.example.toml` に members の例を追記。

## API(`src/main.rs`)

- 新規エンドポイントなし。
- `/api/devices` の各デバイス情報に `members`(名前配列)を追加。空のときは
  フィールドごと省略する(`skip_serializing_if`)。
- メンバーの `/api/devices/{name}/state|on|off|color|brightness|preset` は
  従来どおり全部生きる(展開内タイルがそのまま使う)。

## UI(`index.html`)

- members が非空の light はグループカードとして描画:
  既存の living_lights タイルに「個別に操作 ▾」トグル(シャッターグループと
  同じ流儀)を追加し、展開内にメンバーの**フル機能タイル**(既存の light
  タイル生成を再利用。色・明るさ・プリセット込み)を並べる。
- members に挙がったデバイスはトップの照明一覧から除外する。
- グループカードの状態表示は現状どおり代表ノード読みのまま。
  全メンバーポーリングはしない(exec 直列化で 10 秒級になるため)。
- 展開状態はページ内トグルのみ(永続化しない。シャッターと同じ)。

## エラー処理

- config 不正は起動時に既存スタイルのエラーメッセージで拒否。
- 実行時の失敗系は既存の light 挙動を変えない
  (送信結果のみ正直に返す+押下 ~2 秒後の非同期追いつき取得)。

## テスト

- `config.rs` ユニットテスト: members のパース 1 ケース+検証 4 ケース
  (未知名・非 light・入れ子・重複所属)。
- UI はヘッドレス Chromium のスクリーンショットで目視確認
  (折りたたみ時・展開時)。

## デプロイ

- jarvis-iac の `roles/mando/files/config.toml` の living_lights に
  `members` 6 台を追記し、mando バイナリ更新(`task deploy HOST=jarvis`)と
  併せて適用する。

## やらないこと

- `[[group]]` の汎用化(シャッター専用のまま)。
- グループカードでの全メンバー状態集計。
- 展開状態の永続化。
