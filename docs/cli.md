# サーバ起動・インデックス構築（CLI直接実行）・テスト

## サーバ起動・インデックス構築（CLI直接実行）

普段はサーバの起動も取り込みも、`message-viewer.exe` のダブルクリックと Web UI で行える（詳細は [docs/batch-files.md](batch-files.md) を参照）。コマンドラインから実行する場合は以下。

設定ファイルは `--config` で指定する（省略時は実行ファイルと同じ場所の `config-dev.json` → `config.json`）。

```powershell
.\message-viewer.exe                                   # サーバ起動
.\message-viewer.exe index                             # 直近 incremental_months ヶ月分の差分取り込み（既定。--incremental と同じ）
.\message-viewer.exe index --full                      # 全件走査
.\message-viewer.exe index --rebuild                   # index.db を作り直して全件走査（サーバを止めてから）
.\message-viewer.exe index --only-member "メンバー名"          # 特定メンバーのみ（直近 incremental_months ヶ月分）
.\message-viewer.exe index --full --only-member "メンバー名"   # 同上、全期間
.\message-viewer.exe index --no-thumbs                 # サムネ生成をスキップ
.\message-viewer.exe --config D:\other\config.json      # 別の設定ファイルで起動
```

- 差分取り込みの期間は設定の `incremental_months`（配布版の既定は2）。ただし、まだ取り込んだことの無いメンバーは差分取り込みでも全期間を走査する
- `--rebuild` が作り直すのは `index.db`（メッセージと検索索引）だけ。`user.db`（お気に入り・コメント・非表示・並び順）とサムネイルは残る。
  - サーバ起動中（設定のポートが使用中）は実行できない（サーバが `index.db` を開いているため）。サーバを止めてから実行する
  - `--only-member` とは併用できない（DB 全体を消すため、他のメンバーまで消えてしまう）。特定メンバーを取り込み直すには `--full --only-member` を使う

取り込みは `data\.index.lock` の OS ファイルロックで排他されるため、サーバ内の取り込み・メンバー削除・初期化と CLI の取り込みが同時に走ることはない。

終了コード（`index`）:

| コード | 意味 |
| --- | --- |
| 0 | 成功 |
| 1 | 失敗（設定ファイルを読めない、`root` 未設定、`--rebuild` の拒否・DB を削除できない など） |
| 2 | 既に取り込みなどが実行中（`.index.lock` を取れなかった） |

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
| `api_media_admin.rs` | `/media`・`/thumbs` の配信（Range・キャッシュ）、取り込み、メンバー削除、初期化、CLI の `index --rebuild` |

[← READMEに戻る](../README.md)
