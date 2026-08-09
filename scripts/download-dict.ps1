# 下载白霜拼音词库（rime-frost，GPL-3.0）到 data\rime-frost\cn_dicts\。
# 数据仅用于本地构建 input.imedic：不入 git 仓库（data/ 已 gitignore）、不编进二进制；
# 发布包若含编译产物，需附 GPL-3.0 NOTICE 声明（见 docs/plan/30-conventions.md §6）。
# 幂等：目标已存在且非空则跳过。用法：scripts\download-dict.ps1
$ErrorActionPreference = "Stop"

$base = "https://raw.githubusercontent.com/gaboolic/rime-frost/master/cn_dicts"
$files = @(
    "8105.dict.yaml",
    "41448.dict.yaml",
    "base.dict.yaml",
    "ext.dict.yaml",
    "others.dict.yaml"
)
$dir = Join-Path $PSScriptRoot "..\data\rime-frost\cn_dicts"

if (-not (Test-Path $dir)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

foreach ($f in $files) {
    $dest = Join-Path $dir $f
    if ((Test-Path $dest) -and ((Get-Item $dest).Length -gt 0)) {
        Write-Host "跳过（已存在）: $f"
        continue
    }
    $url = "$base/$f"
    Write-Host "下载: $url"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest
    if ((Get-Item $dest).Length -eq 0) {
        throw "下载失败或文件为空: $url"
    }
}
Write-Host "完成: 词库已下载到 $dir"
