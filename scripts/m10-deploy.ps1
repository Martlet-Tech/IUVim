# M10 测试部署：构建 → dev-deploy 热替换（DLL/词库/简繁表/daemon/注册/ctfmon）
# → 部署并启动 iuv-server.exe。需管理员权限（自动弹 UAC 提权）。
#
# 用法：scripts\m10-deploy.ps1              # 构建四产物 + 部署 + 启动服务端
#       scripts\m10-deploy.ps1 -SkipBuild   # 跳过构建，只部署现有产物
#       scripts\m10-deploy.ps1 -NoServer    # 只部署 TSF/daemon，不启动 iuv-server
#                                           #（本地引擎模式基线回归用）
#
# A/B 开关（49 号 P3b）：config.json 加 "use_engine_server": true → TSF 连 iuv-server
#（薄客户端）；缺省/false = 现状本地引擎（基线）。改完重启输入法（Ctrl+Space 切换）生效。
#requires -Version 5.1

param(
    [switch]$SkipBuild,
    [switch]$NoServer
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
Exit-IfNotAdmin -ScriptPath $PSCommandPath -PassArgs @()

Write-Host "=== M10 测试部署 ==="

# ---- 1. 构建（四车道；-SkipBuild 跳过）----
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'm10-build.ps1')
    if ($LASTEXITCODE -ne 0) { throw "m10-build 失败" }
} else {
    Write-Host "-SkipBuild：跳过构建"
}

# ---- 2. dev-deploy 热替换（TSF DLL x64+x86 / 词库 / 简繁表 / daemon / 注册 / ctfmon）----
# 产物已由 m10-build 备齐，恒 -SkipBuild（dev-deploy 内部产物检查兜底）。
& (Join-Path $PSScriptRoot 'dev-deploy.ps1') -SkipBuild
if ($LASTEXITCODE -ne 0) { throw "dev-deploy 失败（exit=$LASTEXITCODE）" }

$repoRoot  = Split-Path -Parent $PSScriptRoot
$destDir   = Join-Path $env:ProgramFiles "iuv"
$serverSrc = Join-Path $repoRoot "target-server\release\iuv-server.exe"
$serverDst = Join-Path $destDir "iuv-server.exe"
$dictDir   = Join-Path $env:LOCALAPPDATA "iuv"
$configPath = Join-Path $dictDir "config.json"

# ---- 3. iuv-server 部署（引擎服务进程；-NoServer 跳过）----
if ($NoServer) {
    Write-Host "-NoServer：跳过 iuv-server 部署（本地引擎模式基线回归）"
} else {
    if (-not (Test-Path $serverSrc)) {
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
    Write-Host "已部署引擎服务：$serverDst"

    # 启动（后台隐藏窗口；日志 %TEMP%\iuv-server.log）。
    Start-Process -FilePath $serverDst -WorkingDirectory $destDir -WindowStyle Hidden
    Start-Sleep -Milliseconds 800
    if (Get-Process -Name "iuv-server" -ErrorAction SilentlyContinue) {
        Write-Host "iuv-server 已启动（日志：%TEMP%\iuv-server.log）"
    } else {
        Write-Host "警告：iuv-server 启动后退出，请查看 %TEMP%\iuv-server.log"
    }
}

# ---- 4. A/B 开关指引 ----
Write-Host ""
Write-Host "==================== M10 A/B 测试指引 ===================="
Write-Host "配置文件：$configPath"
Write-Host ""
Write-Host '【基线回归】确保配置无 "use_engine_server": true（或缺省），'
Write-Host "  重启输入法后正常打字——行为应与 main 完全一致。"
if (-not $NoServer) {
    Write-Host ""
    Write-Host "【远端模式】在配置 JSON 顶层加一行："
    Write-Host '    "use_engine_server": true,'
    Write-Host "  然后 Ctrl+Space 切走再切回（或重启输入法），打字验证："
    Write-Host "  拼音预编辑/候选窗/空格上屏/Esc 取消/翻页/左右选候选/Shift 中英/"
    Write-Host "  Ctrl+Space/点简繁全半角。杀掉 iuv-server 进程 → 应回落纯英文（透明降级），"
    Write-Host "  应用绝不卡死。"
    Write-Host ""
    Write-Host "【切回本地】把该行改回 false（或删除），重启输入法即可，无需卸载。"
}
Write-Host ""
Write-Host "日志：%TEMP%\iuv-tsf.log / %TEMP%\iuv-server.log"
Write-Host "=========================================================="
