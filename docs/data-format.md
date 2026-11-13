# データ仕様

- 保存フォルダ（`root`）配下は次の4パターンに対応（`{グループ}/` は省略可、`{YYYY}/{YYYYMM}/` も省略可）
  - `{メンバー}/{YYYY}/{YYYYMM}/{file}`
  - `{グループ}/{メンバー}/{YYYY}/{YYYYMM}/{file}`
  - `{メンバー}/{file}`
  - `{グループ}/{メンバー}/{file}`
- ファイル名: `{msg_id}_{flag}_{YYYYMMDDHHMMSS}.{ext}`（flag: 0=text 1=image 2=video 3=audio）
- 主キーは `(member, msg_id)` の複合キー（メンバー間でmsg_idが衝突するため）
- 画像・動画の場所は `root` からの相対パスでDBに保存する。取り込み後にフォルダ構成を変えた場合は全件走査で更新する必要がある（詳細は [docs/setup.md](setup.md) 参照）
- 削除されたファイルはDBから消さず `missing=1` を立てる

同一メンバー内で `{YYYY}/{YYYYMM}/` 配下と直下ファイルが混在していても、どちらも取り込む（詳細は [docs/setup.md](setup.md) 参照）。

[← READMEに戻る](../README.md)
