# サーバ起動・インデックス構築（CLI直接実行）・テスト

## サーバ起動・インデックス構築（CLI直接実行）

Windowsでは上記バッチファイルが基本。WSL/Linuxまたは手動実行の場合は以下。

```bash
cd /message-viewer
source venv/bin/activate

python3 app.py
# または: uvicorn app:app --host 127.0.0.1 --port 8000

python3 indexer.py --incremental       # 直近2ヶ月分のみ差分取り込み（既定）
python3 indexer.py --full              # 全件走査
python3 indexer.py --rebuild           # DBを作り直して全件走査
python3 indexer.py --only-member "メンバー名"          # 特定メンバーのみ（直近2ヶ月分）
python3 indexer.py --full --only-member "メンバー名"   # 同上、全期間
python3 indexer.py --incremental --no-thumbs           # サムネ生成をスキップ
```

`auto_index_on_startup: true` の場合、起動時にバックグラウンドで差分取り込みが自動実行される（サーバ応答はブロックしない）。

ブラウザで `http://localhost:8000/` を開く（`static/index.html` にCSS/JSをインラインで実装した単一ファイル構成）。

## テスト

```bash
source venv/bin/activate
pip install -r requirements-dev.txt
pytest
```

`test/` 配下に `config.py` / `indexer.py` / `app.py`（FastAPIの `TestClient` 経由）のテストがある。すべて `tmp_path` 上に隔離したconfig.json・DB・rootフォルダを使うため、本番の `config.json` や `data/` には影響しない。

[← READMEに戻る](../README.md)
