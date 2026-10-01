# 卸载 iuv 输入法（赶尽杀绝）：停服务 → 删注册表键 → 重启 ctfmon → 删文件 → 自检。
# 需管理员权限（自动弹 UAC 提权）。用法：scripts\uninstall.ps1
#
# 卸载的完成形态：系统输入法列表里看不到 iuv、iuv-server 服务与自启任务消失、
# 文件（含被占用残留）清理干净。DLL 被运行中应用映射属输入法常态——残留交给
# 延迟清理（注销/重启后自动执行），**注销一次收尾不可避免**。
#
# 设计要点：
# - 不调用 regsvr32 /u（会加载 DLL，被占用时挂起）；注册键直接删，效果相同。
# - 不杀 explorer、不要求关闭应用：DLL 被占用只影响文件删除，不影响注册表与列表刷新。
# - 用户数据（config.json / 用户词库）删除前备份，误卸可恢复；共享密钥随之失效属预期。
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
Exit-IfNotAdmin -ScriptPath $PSCommandPath

Trace-Script "uninstall: 提升实例启动"
Write-Host "正在卸载 IUV 输入法（管理员窗口）..."

$p = Get-IuvPaths
$destDir = $p.DestDir
$clsid = '{C69735F1-BAB1-458B-89FC-099ABA877ECB}'

# ---- 0. 停服务进程 + 注销登录自启 ----
if (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue) {
    Stop-IuvServerGraceful | Out-Null
}
# 历史安装残留（daemon 已退役，2026-10 并入 server）：存在即杀。
Stop-Process -Name 'iuv-daemon' -Force -ErrorAction SilentlyContinue
try {
    Unregister-ScheduledTask -TaskName 'Iuv-ServerStart' -Confirm:$false -ErrorAction Stop
    Write-Host "已注销登录自启任务 Iuv-ServerStart"
} catch {
    Write-Host "自启任务 Iuv-ServerStart 不存在，跳过注销"
}
Start-Sleep -Milliseconds 300

# ---- 0.5 备份用户数据（config.json + 用户词库；重装后拷回即恢复个人设置）----
$backupDir = Join-Path $env:LOCALAPPDATA 'iuv-backup'
$backedUp = @()
foreach ($f in @('config.json', 'iuv.user.imedic')) {
    $src = Join-Path $p.DictDir $f
    if (Test-Path $src) {
        New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
        Copy-Item $src (Join-Path $backupDir $f) -Force
        $backedUp += $f
    }
}
if ($backedUp.Count -gt 0) {
    Write-Host "已备份用户数据到 $backupDir ：$($backedUp -join ', ')"
}

# 本输入法注册的键（对应 crates/iuv-tsf/src/registration.rs）：
# 1) HKCR\CLSID\{GUID}             COM 类注册（DllRegisterServer 写，x64 native 视图）
# 2) HKLM\...\CTF\TIP\{GUID}       TSF 文本服务注册（ITfInputProcessorProfiles 写，x64 native 视图）
# 3) HKCU\...\CTF\TIP\{GUID}       无管理员权限时 TSF 注册可能落到用户级
# 4) HKLM\...\Classes\WOW6432Node\CLSID\{GUID}    x86 DLL 注册（32 位 regsvr32 落 WoW64 视图）
# 5) HKLM\...\WOW6432Node\...\CTF\TIP\{GUID}      x86 TSF 注册（32 位 TSF 管理器落 WoW64 视图）
$keys = @(
    "Registry::HKEY_CLASSES_ROOT\CLSID\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\$clsid",
    "Registry::HKEY_CURRENT_USER\SOFTWARE\Microsoft\CTF\TIP\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\WOW6432Node\CLSID\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\CTF\TIP\$clsid"
)

# ---- 1. 删除注册表键 ----
foreach ($k in $keys) {
    if (Test-Path $k) {
        Remove-Item -LiteralPath $k -Recurse -Force
        Trace-Script "uninstall: 删除注册表键 $k"
        Write-Host "已删除注册表键：$k"
    }
}
Trace-Script "uninstall: 注册表清理完成"
Write-Host "注册表清理完成。"

# ---- 2. 重启 ctfmon：注册表已无本输入法，ctfmon 不再加载 DLL，托盘列表刷新 ----
if (-not (Restart-Ctfmon)) {
    Trace-Script "uninstall: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请手动重启（任务管理器结束 ctfmon.exe 后，新建任务运行 ctfmon.exe），或注销/重启后输入法列表自动刷新。"
} else {
    Trace-Script "uninstall: ctfmon 已重启"
}

# ---- 3. 删除安装文件与用户数据（未锁直接删，有残留交给延迟清理）----
$delayed = $false
if (Test-Path $destDir) {
    $locked = @()
    Get-ChildItem -LiteralPath $destDir -Force -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.PSIsContainer) { return }
        $file = $_.FullName
        try { Remove-Item -LiteralPath $file -Force -ErrorAction Stop }
        catch { $locked += $file }
    }
    if ($locked.Count -gt 0) { Trace-Script ("uninstall: 发现锁定文件 " + ($locked -join ', ')) }
    # 目录：未锁文件删完后若已空则删除；否则保留（延迟清理连壳删掉）。
    try { Remove-Item -LiteralPath $destDir -Recurse -Force -ErrorAction Stop } catch {}
    if ($locked.Count -eq 0 -and -not (Test-Path $destDir)) {
        Trace-Script "uninstall: 安装目录已删除"
        Write-Host "已删除安装目录：$destDir"
    } else {
        $delayed = $true
    }
}

$userData = Join-Path $env:LOCALAPPDATA "iuv"
if (Test-Path $userData) {
    try {
        Remove-Item -LiteralPath $userData -Recurse -Force -ErrorAction Stop
        Trace-Script "uninstall: 删除用户数据 $userData"
        Write-Host "已删除用户数据（词库）：$userData"
    } catch {
        $delayed = $true
    }
}

# 旧版本残留（改名前的 Input IME 安装，%ProgramFiles%/%LOCALAPPDATA%/%ProgramData%）：顺带清理。
$legacyDirs = @(
    (Join-Path $env:ProgramFiles "InputIME"),
    (Join-Path $env:LOCALAPPDATA "InputIME"),
    (Join-Path $env:ProgramData "InputIME")
)
foreach ($legacyDir in $legacyDirs) {
    if (Test-Path $legacyDir) {
        try { Remove-Item -LiteralPath $legacyDir -Recurse -Force -ErrorAction Stop } catch {}
        if (Test-Path $legacyDir) {
            Trace-Script "uninstall: 旧版残留未删净 $legacyDir"
            $delayed = $true
        } else {
            Write-Host "已清理旧版残留：$legacyDir"
        }
    }
}

# ---- 4. 残留：双保险登记延迟清理 ----
if ($delayed) {
    $paths = @($destDir, $userData) + @($legacyDirs | Where-Object { Test-Path -LiteralPath $_ })
    Trace-Script ("uninstall: 残留路径待延迟清理 [" + ($paths -join ';') + "]")
    # 双保险：无前缀 PendingFileRenameOperations（重启时 SmSs 无条件执行）+ 计划任务（注销/重启触发）
    foreach ($path in $paths) { Add-PendingOp -Source $path }
    if (Register-DelayedOps -Deletes $paths) {
        Write-Host "以下残留正被占用，已安排自动清理（注销或重启后生效，无需手动操作）："
        $paths | ForEach-Object { Write-Host "  $_" }
    } else {
        Write-Host "警告：自动清理任务注册失败，请注销/重启后手动删除：$($paths -join ', ')"
    }
}

# ---- 5. 自检 ----
$left = @($keys | Where-Object { Test-Path $_ })
if ($left.Count -gt 0) {
    Trace-Script "uninstall: 残留注册表键 $($left -join ';')"
    Write-Host "警告：仍有残留注册表键：$($left -join ', ')"
    exit 1
}
if ($delayed) {
    if (Get-ScheduledTask -TaskName Iuv-DelayedOps -ErrorAction SilentlyContinue) {
        Trace-Script "uninstall: 完成（延迟清理任务已注册）"
        Write-Host ""
        Write-Host "卸载完成。残留文件将在注销或重启后自动清理（注销一次收尾属输入法卸载常态）。"
    } else {
        Trace-Script "uninstall: 完成但延迟清理任务未确认"
        Write-Host ""
        Write-Host "卸载完成。注意：延迟清理任务未确认，如有残留请手动删除。"
    }
} else {
    Trace-Script "uninstall: 完成（无残留）"
    Write-Host ""
    Write-Host "卸载完成，已完全清除。"
}
if ($backedUp.Count -gt 0) {
    Write-Host "个人设置/用户词备份在：$backupDir（重装后把其中的文件拷回 $($p.DictDir) 即可恢复）"
}
