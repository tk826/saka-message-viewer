# セットアップ詳細

## 事前準備: Pythonのインストール

[python.org](https://www.python.org/downloads/) からWindows版Pythonをインストールする（Microsoft Store版は動作対象外。理由は後述の「セットアップ（Windows ネイティブ・推奨）」を参照）。インストール時は「Add python.exe to PATH」にチェックを入れておくと、後続のセットアップで実体パスの指定が不要になる。

WSL/Linux側で動かす場合は、ディストリのパッケージマネージャ（`apt install python3` 等）でPython 3.xを用意する。

## 事前準備: `MoveFiles` によるファイル整形（必須）

メッセージ保存フォルダ（`root`）配下は `{メンバー}/{YYYY}/{YYYYMM}/{file}` または `{グループ}/{メンバー}/{YYYY}/{YYYYMM}/{file}` の階層である必要がある。
**`{メンバー}` 直下に置かれたファイル（`{YYYY}/{YYYYMM}` を経由しないもの）は `indexer.py` の走査対象外**で、静かに無視される。この階層は差分取り込みをディレクトリ名単位で絞り込むために必須で、無いと実データ規模（1メンバーあたり数万ファイル）で差分取り込みが大幅に遅くなる。

`MoveFiles.bat`（実体は `script_ps1\MoveFiles.ps1`）は、対象ディレクトリを再帰走査し、ファイル名のタイムスタンプから `{YYYY}/{YYYYMM}/` サブフォルダを作ってファイルを移動する前処理ツール。**取り込み前に必ず実行しておく。**

```powershell
MoveFiles.bat "C:\messages"
```

- 対象パターン: `{msg_id(5〜6桁)}_{flag}_{YYYYMMDDHHMMSS}.{jpg|mp4|txt}`
- 移動済みファイル・移動先に同名ファイルが既にある場合はスキップ（再実行しても安全）

## セットアップ（Windows ネイティブ・推奨）

`/mnt/*` 経由のI/OはWSLからだと非常に遅いため、Windows Pythonをネイティブに使う。

- **PATH上の `python` が Microsoft Store のスタブだと動かない。** フルパスの実体を使うこと（例: `C:\Users\<ユーザー名>\AppData\Local\Programs\Python\Python314\python.exe`）
- 日本語パスを扱うため、実行は `.ps1`（UTF-8 with BOM）経由に統一している

`setup_venv_win.bat`（実体は `script_ps1\setup_venv_win.ps1`）が `venv_win`（WSL用の`venv/`とは別ディレクトリ）の作成と依存導入をまとめて行う。ダブルクリックで実行可能。

```powershell
cd \message-viewer
.\setup_venv_win.bat

# PATH上のpythonが使えない場合はps1を直接、実体パスを指定して実行
.\script_ps1\setup_venv_win.ps1 -PythonPath "C:\Users\<ユーザー名>\AppData\Local\Programs\Python\Python314\python.exe"
```

## セットアップ（WSL / Linux）

```bash
cd /message-viewer
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
```

WSL/Windows双方から使う場合、`data_dir`等は実行するPythonのOSに応じて自動解決されるが、DBファイル（`data/index.db`）はOSごとのリポジトリ配置が別だと共有されない。共有したい場合はWSL側もWindowsドライブをマウントしたパス（例: `/mnt/c/message-viewer`）に配置する。

[← READMEに戻る](../README.md)
