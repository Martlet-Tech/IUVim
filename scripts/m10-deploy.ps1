# M10 测试部署：构建（当前窗口）→ UAC 提权 → dev-deploy 热替换 + iuv-server 部署启动。
#
# 用法：scripts\m10-deploy.ps1              # 构建 + 部署 + 启动服务端
#       scripts\m10-deploy.ps1 -SkipBuild   # 跳过构建，只部署现有产物
#       scripts\m10-deploy.ps1 -NoServer    # 只部署 TSF，不启动 iuv-server（M10 ②：daemon 已退役）
#                                           #（本地引擎模式基线回归用）
#
# 设计要点（实测教训）：**构建在当前（普通）窗口执行**——UAC 提权进程可能换了
# 管理员账户/丢失用户 PATH（找不到 cargo），构建在提权上下文里会瞬间失败且窗口
# 闪退无法看到错误。提权只做部署（部署不需要 cargo），失败信息落在
# %TEMP%\iuv-script.log（dev-deploy 的 Trace-Script）。
#
# A/B 开关（49 号 P3b）：config.json 加 "use_engine_server": true → TSF 连 iuv-server
#（薄客户端）；缺省/false = 现状本地引擎（基线）。改完重启输入法（Ctrl+Space 切换）生效。
#requires -Version 5.1

param(
    [switch]$SkipBuild,
    [switch]$NoServer
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot

# ---- 1. 构建（当前窗口；普通权限即可）----
# 注意：调用 .ps1 子脚本后**不可**用 $LASTEXITCODE 判成败——子脚本成功时不设置它，
# 查到的是 shell 里上一次原生命令的残留值（实测假失败）。子脚本失败走 throw → catch。
if (-not $SkipBuild) {
    try {
        & (Join-Path $PSScriptRoot 'm10-build.ps1')
    } catch {
        throw "m10-build 失败：$_"
    }
} else {
    Write-Host "-SkipBuild：跳过构建"
}

# ---- 2. 提权执行部署部分（部署不需要 cargo；提权后丢 PATH 也不影响）----
# 本脚本被提权重拉时带 -SkipBuild（构建已在发起窗口完成）。
$pass = @('-SkipBuild')
if ($NoServer) { $pass += '-NoServer' }

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "构建完成，弹出 UAC 提权窗口执行部署（部署日志：`$env:TEMP\iuv-script.log）..."
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"") + $pass
    try {
        Start-Process powershell -Verb RunAs -Wait -ArgumentList $argList
    } catch {
        Write-Host "错误：UAC 提权被取消或失败（$_）。"
        exit 1
    }
    exit 0
}

# ========== 以下为管理员上下文 ==========
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
Trace-Script "m10-deploy: 管理员实例启动（SkipBuild=$SkipBuild NoServer=$NoServer）"
Write-Host "=== M10 测试部署（管理员）==="

# ---- 3. dev-deploy 热替换（TSF DLL x64+x86 / 词库 / 简繁表 / 注册 / ctfmon；daemon 已退役）----
# 成败不查 $LASTEXITCODE（残留值问题同上），直接验产物：目标 DLL 不旧于源产物。
$destDllCheck = Join-Path $env:ProgramFiles "iuv\iuv_tsf.dll"
try {
    & (Join-Path $PSScriptRoot 'dev-deploy.ps1') -SkipBuild
} catch {
    Trace-Script "m10-deploy: dev-deploy 异常：$_"
    throw "dev-deploy 失败：$_，详见 %TEMP%\iuv-script.log"
}
$srcDll = Join-Path $repoRoot "target\release\iuv_tsf.dll"
$srcTime = (Get-Item $srcDll).LastWriteTime
$dstTime = (Get-Item $destDllCheck -ErrorAction SilentlyContinue).LastWriteTime
if (-not $dstTime -or $dstTime -lt $srcTime.AddSeconds(-1)) {
    Trace-Script "m10-deploy: dev-deploy 产物校验失败（dst=$dstTime src=$srcTime）"
    throw "部署校验失败：$destDllCheck 未更新（详见 %TEMP%\iuv-script.log）"
}
Trace-Script "m10-deploy: dev-deploy 完成（产物已更新）"

$destDir   = Join-Path $env:ProgramFiles "iuv"
$serverSrc = Join-Path $repoRoot "target-server\release\iuv-server.exe"
$serverDst = Join-Path $destDir "iuv-server.exe"

# ---- 4. iuv-server 部署（引擎服务进程；-NoServer 跳过）----
if ($NoServer) {
    Write-Host "-NoServer：跳过 iuv-server 部署（本地引擎模式基线回归）"
} else {
    if (-not (Test-Path $serverSrc)) {
        Trace-Script "m10-deploy: 错误，server 产物缺失 $serverSrc"
        Write-Host "错误：未找到 $serverSrc（m10-build 的 server 车道产物）"
        exit 1
    }
    # 停旧实例（复制会锁；密钥文件共享，重启无缝）。
    $old = Get-Process -Name "iuv-server" -ErrorAction SilentlyContinue
    if ($old) {
        Write-Host "停止运行中的 iuv-server（PID=$($old.Id -join ',')）..."
        Stop-Process -Name "iuv-server" -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }
    Copy-Item $serverSrc $serverDst -Force -ErrorAction Stop
    Trace-Script "m10-deploy: iuv-server 复制成功 $serverDst"
    Write-Host "已部署引擎服务：$serverDst"

    # 启动（**受限计划任务**，同 Restart-Ctfmon 模式）。**必须**在用户的中完整性
    # 上下文运行：提权脚本直接 Start-Process 会创建**高完整性**管道，而 TSF 客户端
    # 全部跑在普通应用的中完整性进程里 → 连接 error 5 拒绝访问（实测 2026-09-27）。
    # ExecutionTimeLimit 清零：计划任务默认 3 天限时会杀长驻服务进程。
    $tn = 'Iuv-ServerStart'
    try {
        $u = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        $action = New-ScheduledTaskAction -Execute $serverDst -WorkingDirectory $destDir
        $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddSeconds(5)
        $principal = New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited
        $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero)
        Register-ScheduledTask -TaskName $tn -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force -ErrorAction Stop | Out-Null
        Start-ScheduledTask -TaskName $tn -ErrorAction Stop
    } catch {
        Trace-Script "m10-deploy: 计划任务启动失败：$_"
        Write-Host "警告：计划任务启动失败（$_），回退直启（可能高完整性不可连）"
        Start-Process -FilePath $serverDst -WorkingDirectory $destDir -WindowStyle Hidden
    }
    try { Unregister-ScheduledTask -TaskName $tn -Confirm:$false -ErrorAction SilentlyContinue } catch {}
    Start-Sleep -Milliseconds 800
    if (Get-Process -Name "iuv-server" -ErrorAction SilentlyContinue) {
        Trace-Script "m10-deploy: iuv-server 已启动（用户中完整性上下文）"
        Write-Host "iuv-server 已启动（用户上下文；日志：%TEMP%\iuv-server.log）"
    } else {
        Trace-Script "m10-deploy: iuv-server 启动后退出"
        Write-Host "警告：iuv-server 启动后退出，请查看 %TEMP%\iuv-server.log"
    }
}

# ---- 5. A/B 开关指引 ----
Write-Host ""
Write-Host "==================== M10 A/B 测试指引 ===================="
Write-Host '【基线回归】确保配置无 "use_engine_server": true（或缺省），'
Write-Host "  重启输入法后正常打字——行为应与 main 完全一致。"
if (-not $NoServer) {
    Write-Host ""
    Write-Host "【远端模式】config.json 顶层加一行后重启输入法："
    Write-Host '    "use_engine_server": true,'
    Write-Host "  打字验证：拼音预编辑/候选窗/空格上屏/Esc 取消/翻页/左右选候选/Shift 中英/"
    Write-Host "  Ctrl+Space/点简繁全半角。杀掉 iuv-server 进程 → 应回落纯英文（透明降级），"
    Write-Host "  应用绝不卡死。"
    Write-Host ""
    Write-Host "【切回本地】把该行改回 false（或删除），重启输入法即可，无需卸载。"
}
Write-Host ""
Write-Host "日志：%TEMP%\iuv-tsf.log / %TEMP%\iuv-server.log / %TEMP%\iuv-script.log"
Trace-Script "m10-deploy: 完成"
Write-Host "=========================================================="
