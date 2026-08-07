# データ仕様

- 保存フォルダ（`root`）配下: `{メンバー}/{YYYY}/{YYYYMM}/{file}` または `{グループ}/{メンバー}/{YYYY}/{YYYYMM}/{file}`
- ファイル名: `{msg_id}_{flag}_{YYYYMMDDHHMMSS}.{ext}`（flag: 0=text 1=image 2=video 3=audio）
- 主キーは `(member, msg_id)` の複合キー（メンバー間でmsg_idが衝突するため）
- 削除されたファイルはDBから消さず `missing=1` を立てる

取り込み対象にするには事前に `{YYYY}/{YYYYMM}/` 配下へ移動しておく必要がある（詳細は [docs/setup.md](setup.md) 参照）。

[← READMEに戻る](../README.md)
