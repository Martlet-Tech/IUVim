# 下载 OpenCC 简→繁转换表（BYVoid/OpenCC，Apache-2.0）到 data\opencc\。
# 数据仅用于本地构建 iuv.opencc（31-script-traditional.md）：不入 git 仓库（data/ 已 gitignore）、
# 不编进二进制；发布包若含编译产物 iuv.opencc，需附 Apache-2.0 NOTICE 声明（见 docs/plan/02-conventions.md §6）。
# 幂等：目标已存在且非空则跳过。用法：scripts\download-opencc.ps1
$ErrorActionPreference = "Stop"

$base = "https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary"
$files = @(
    "STPhrases.txt",
    "STCharacters.txt"
)
$dir = Join-Path $PSScriptRoot "..\data\opencc"

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
Write-Host "完成: OpenCC 转换表已下载到 $dir"
Write-Host "编译: cargo run -p iuv-data --bin dictc -- opencc data\iuv.opencc data\opencc\STPhrases.txt data\opencc\STCharacters.txt"