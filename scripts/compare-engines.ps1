# 双引擎对拍脚本（39-rime-pipeline.md §9）：classic vs rime 候选并排输出。
# 用法：scripts/compare-engines.ps1 [-Dict data\iuv.imedic] [-Words nihao,xian,nhmsx]
param(
    [string]$Dict = "data\iuv.imedic",
    [string]$Words = "nihao,xian,shigechengy,nhmsx,nhao,nihaoshijie,sh,zheshiming"
)
$repl = ".\target\debug\iuv-repl.exe"
if (-not (Test-Path $repl)) { Write-Error "先构建：cargo build -p iuv-repl"; exit 1 }
foreach ($w in $Words.Split(",")) {
    Write-Host "===== $w =====" -ForegroundColor Cyan
    $c = & $repl $Dict --batch $w 2>$null | Select-Object -Skip 1 -First 5
    $r = & $repl $Dict --engine rime --batch $w 2>$null | Select-Object -Skip 1 -First 5
    Write-Host "[classic]" -ForegroundColor Yellow; $c | ForEach-Object { Write-Host "  $_" }
    Write-Host "[rime]   " -ForegroundColor Green;  $r | ForEach-Object { Write-Host "  $_" }
}
