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

# ---- 1. 停服务进程（iuv-server 走优雅停机（广播 Push::Shutdown）；iuv-daemon 强杀残留）----
if (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue) {
    Stop-IuvServerGraceful | Out-Null
}
$p = Get-Process -Name 'iuv-daemon' -ErrorAction SilentlyContinue
if ($p) {
    Write-Host "停止 iuv-daemon（PID=$($p.Id -join ',')）..."
    Stop-Process -Name 'iuv-daemon' -Force -ErrorAction SilentlyContinue
}
Start-Sleep -Milliseconds 300

# ---- 1.5 注销登录自启任务（m10-deploy 常驻注册的 AtLogOn server 自启）----
try {
    Unregister-ScheduledTask -TaskName 'Iuv-ServerStart' -Confirm:$false -ErrorAction Stop
    Write-Host "已注销登录自启任务 Iuv-ServerStart"
} catch {
    Write-Host "自启任务 Iuv-ServerStart 不存在，跳过注销"
}

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
# 成败不查 $LASTEXITCODE（子脚本成功不设置它，残留值会假失败）；uninstall 自身
# 有残留自检（残留注册表键时 exit 1 + 输出警告）。
try {
    & (Join-Path $PSScriptRoot 'uninstall.ps1')
} catch {
    throw "uninstall 失败：$_"
}

Write-Host ""
if ($backedUp.Count -gt 0) {
    Write-Host "M10 卸载完成。个人设置/用户词备份在：$backupDir"
    Write-Host "重装后把其中的文件拷回 $dictDir 即可恢复。"
} else {
    Write-Host "M10 卸载完成。"
}
