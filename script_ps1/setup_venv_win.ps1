<#
.SYNOPSIS
  Windowsネイティブ実行用のvenv（venv_win）を作成し、依存パッケージを導入する。

.DESCRIPTION
  PATH上の `python` が Microsoft Store のスタブになっている環境では動かないため、
  その場合はフルパスの実体を -PythonPath で指定する。
  PATH上の `python` が正規の実体を指すよう設定済みであれば -PythonPath は省略可能。
  run_server.ps1 / run_rebuild.ps1 / run_only_member.ps1 はいずれも
  venv_win\Scripts\python.exe を直接呼び出すため、これらを使う前に
  一度だけ実行しておく必要がある。

.PARAMETER PythonPath
  Windows Python実体のフルパス。省略時はPATH上の `python` を使う。
  例: C:\Users\<ユーザー名>\AppData\Local\Programs\Python\Python314\python.exe

.EXAMPLE
  powershell.exe -File setup_venv_win.ps1

.EXAMPLE
  powershell.exe -File setup_venv_win.ps1 -PythonPath "C:\Users\<ユーザー名>\AppData\Local\Programs\Python\Python314\python.exe"
#>

[CmdletBinding()]
param(
    [string]$PythonPath = ""
)

$ErrorActionPreference = "Stop"
chcp 65001 | Out-Null
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:PYTHONUTF8 = "1"
$env:PYTHONIOENCODING = "utf-8"
Set-Location (Split-Path $PSScriptRoot -Parent)

if ($PythonPath -eq "") {
    $found = Get-Command python -ErrorAction SilentlyContinue
    if (-not $found) {
        throw "PATH上に python が見つかりません。-PythonPath でフルパスを指定してください。"
    }
    if ($found.Source -like "*WindowsApps*") {
        throw "PATH上の python は Microsoft Store のスタブです。-PythonPath でフルパスの実体を指定してください。"
    }
    $PythonPath = $found.Source
} elseif (-not (Test-Path $PythonPath)) {
    throw "Python実体が見つかりません: $PythonPath"
}

& $PythonPath -m venv venv_win
.\venv_win\Scripts\python.exe -m pip install --upgrade pip
.\venv_win\Scripts\python.exe -m pip install -r requirements.txt

Write-Host "venv_win の作成が完了しました。" -ForegroundColor Green
