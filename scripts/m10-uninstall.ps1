# M10 测试卸载：停 iuv-server / iuv-daemon → 备份用户词库/配置 → 复用 uninstall.ps1
# 全清（注册表/安装目录/用户数据）。需管理员权限（自动弹 UAC 提权）。
#
# 用法：scripts\m10-uninstall.ps1
#
# 注意：uninstall.ps1 会删除 %LOCALAPPDATA%\iuv（含 config.json / 用户词库 /
# 共享密钥）。本脚本先备份 config.json 与 iuv.user.imedic 到
# %LOCALAPPDATA%\iuv-m10-backup\，卸载后手动拷回即可恢复个人设置与用户词。
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
Exit-IfNotAdmin -ScriptPath $PSCommandPath -PassArgs @()

Write-Host "=== M10 测试卸载 ==="

# ---- 1. 停服务进程（iuv-server 引擎服务 / iuv-daemon 工具栏守护）----
foreach ($name in @('iuv-server', 'iuv-daemon')) {
    $p = Get-Process -Name $name -ErrorAction SilentlyContinue
    if ($p) {
        Write-Host "停止 $name（PID=$($p.Id -join ',')）..."
        Stop-Process -Name $name -Force -ErrorAction SilentlyContinue
    }
}
Start-Sleep -Milliseconds 300

# ---- 2. 备份用户数据（config.json + 用户词库；密钥随之失效属预期——重装自动重新生成）----
$dictDir   = Join-Path $env:LOCALAPPDATA "iuv"
$backupDir = Join-Path $env:LOCALAPPDATA "iuv-m10-backup"
$backedUp = @()
foreach ($f in @('config.json', 'iuv.user.imedic')) {
    $src = Join-Path $dictDir $f
    if (Test-Path $src) {
        New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
        Copy-Item $src (Join-Path $backupDir $f) -Force
        $backedUp += $f
    }
}
if ($backedUp.Count -gt 0) {
    Write-Host "已备份用户数据到 $backupDir ：$($backedUp -join ', ')"
}

# ---- 3. 全清卸载（复用现有 uninstall.ps1：注册表键 + 安装目录 + 用户数据 + 延迟清理）----
& (Join-Path $PSScriptRoot 'uninstall.ps1')
if ($LASTEXITCODE -ne 0) { throw "uninstall 失败（exit=$LASTEXITCODE）" }

Write-Host ""
if ($backedUp.Count -gt 0) {
    Write-Host "M10 卸载完成。个人设置/用户词备份在：$backupDir"
    Write-Host "重装后把其中的文件拷回 $dictDir 即可恢复。"
} else {
    Write-Host "M10 卸载完成。"
}
