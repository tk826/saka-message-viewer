# サーバ起動・インデックス構築（CLI直接実行）・テスト

## サーバ起動・インデックス構築（CLI直接実行）

Windowsでは基本的に `.bat` ファイル経由で実行する（詳細は [docs/batch-files.md](batch-files.md) を参照）。手動で実行する場合は以下。

設定ファイルは `--config` で指定する（省略時は実行ファイルと同じ場所の `config-dev.json` → `config.json`）。

```powershell
.\message-viewer.exe                                   # サーバ起動
.\message-viewer.exe index                             # 直近2ヶ月分のみ差分取り込み（既定）
.\message-viewer.exe index --full                      # 全件走査
.\message-viewer.exe index --rebuild                   # DBを作り直して全件走査
.\message-viewer.exe index --only-member "メンバー名"          # 特定メンバーのみ（直近2ヶ月分）
.\message-viewer.exe index --full --only-member "メンバー名"   # 同上、全期間
.\message-viewer.exe index --no-thumbs                 # サムネ生成をスキップ
.\message-viewer.exe --config D:\other\config.json      # 別の設定ファイルで起動
```

取り込みは `data\.index.lock` で排他されるため、サーバ内の取り込みと CLI の取り込みが同時に走ることはない（既に実行中なら終了コード2で終了する）。

`auto_index_on_startup: true` の場合、起動時にバックグラウンドで差分取り込みが自動実行される（サーバ応答はブロックしない）。

ブラウザで `http://localhost:8000/` を開く（`static/index.html` にCSSをインラインで実装し、JSは `static/js/*.js` にES Modulesとして分割。`static/index.html` の `<script type="module" src="/static/js/app.js">` がエントリポイント。ビルドツールは使わずブラウザネイティブのESMをそのまま配信する）。

## テスト

```powershell
cargo test --manifest-path rust\Cargo.toml
```

| 場所 | 内容 |
| --- | --- |
| `rust/src/**`（`#[cfg(test)]`） | 単体テスト（語の分解・スニペット・日付・Range の解釈・サムネの寸法・ロックなど） |
| `rust/tests/api_*.rs` | HTTP の契約テスト。ビルドしたサーバを一時フォルダで起動し、HTTP だけで検証する |
| `rust/tests/common/mod.rs` | 契約テストの共通部分（サンプルの保存フォルダ・サーバの起動・HTTP クライアント） |
| `rust/tests/fixtures/search_golden.json` | 検索の語の分解・スニペットの期待値 |

契約テストは一時フォルダに設定ファイル・保存フォルダ・`data/` を作って使うため、本番の `config.json` / `config-dev.json` や `data/` には影響しない。

| ファイル | 対象 |
| --- | --- |
| `api_smoke.rs` | 起動・設定・外部アクセス対策（Host・Sec-Fetch-Site・Origin） |
| `api_read.rs` | threads / messages / search / jump / calendar / media-list |
| `api_user.rs` | お気に入り・コメント・非表示メンバー・並び順 |
| `api_media_admin.rs` | `/media`・`/thumbs` の配信（Range・キャッシュ）、取り込み、メンバー削除、初期化 |

[← READMEに戻る](../README.md)
