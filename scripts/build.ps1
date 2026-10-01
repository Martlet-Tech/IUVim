# 构建全部产物（无需管理员权限）：三车道并行。
# 车道：x64-tsf（target\）∥ x86-tsf（target\i686-…）∥ server（target-server\）。
# 各车道独立 target 目录互不持锁，并行耗时 = 最长单链。构建逻辑在 iuv-common.ps1
# 的 Build-All（install/dev-deploy 复用同一函数），本脚本只是独立构建入口。
#
# 产物：
#   target\release\iuv_tsf.dll
#   target\i686-pc-windows-msvc\release\iuv_tsf.dll
#   target-server\release\iuv-server.exe
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')

try {
    Build-All
} catch {
    Write-Host "错误：$_"
    exit 1
}
Write-Host ""
Write-Host "构建完成（三产物齐全）。下一步：scripts\dev-deploy.ps1（热部署）或 scripts\install.ps1（全新安装）"
