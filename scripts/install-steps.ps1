# 安装包内的「装后动作」（61 号）：注册计划任务 → 起 server → 生成默认配置 → 重启 ctfmon。
#
# 为什么这些不放 Inno 的 [Code] 里：
#  · 计划任务的「崩溃重启」(RestartOnFailure) 在 schtasks 命令行没有参数，只能写 XML；
#    而 Inno 的 Pascal 脚本没有 UTF-16 写文件的干净手段（TStringList.SaveToFile 是 ANSI，
#    传 TEncoding.Unicode 直接编译报错）。硬写 XML 还会与本仓库 PowerShell 侧
#    （scripts\iuv-common.ps1 的 Deploy-IuvServer）形成**两份真相**，早晚漂移。
#  · 这里**直接复用** iuv-common.ps1 的已验证函数，与 dev-deploy / install 走同一套逻辑。
#
# 用法（Inno [Run] 调用，已提权）：powershell -File install-steps.ps1
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')

$p = Get-IuvPaths
Trace-Script "install-steps: 开始（app=$($p.DestDir)）"

if (-not (Test-Path $p.ServerDst)) {
    Trace-Script "install-steps: 缺少 $($p.ServerDst)"
    Write-Host "错误：未找到 $($p.ServerDst)（安装包不完整？）"
    exit 1
}

# ---- 1. 计划任务：登录自启 + 周期自愈（与 Deploy-IuvServer 同参数）----
# 单独重写而不是调 Deploy-IuvServer：那条路径会先从 target-server 复制 exe，而安装包里
# exe 已由 Inno 落地。参数必须保持同步——改一处要改两处，两处都带这段注释。
# 崩溃自愈用「1 分钟重复触发器 + IgnoreNew」而非 RestartOnFailure（后者本机 4 轮
# 探针实测从不触发），详见 iuv-common.ps1 同名函数的注释与 61 号 §3.3。
$tn = 'Iuv-ServerStart'
$u = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $p.ServerDst -WorkingDirectory $p.DestDir
$triggers = @(
    (New-ScheduledTaskTrigger -AtLogOn -User $u),
    (New-ScheduledTaskTrigger -Once -At (Get-Date) `
        -RepetitionInterval (New-TimeSpan -Minutes 1) `
        -RepetitionDuration (New-TimeSpan -Days 3650))
)
# RunLevel Limited = 中完整性：提权上下文会让 TSF 客户端连不上管道（2026-09-27 实测）
$principal = New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet `
    -StartWhenAvailable `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $tn -Action $action -Trigger $triggers `
    -Principal $principal -Settings $settings -Force -ErrorAction Stop | Out-Null
Trace-Script "install-steps: 计划任务已注册（AtLogOn $u + 1min 周期自愈）"

# ---- 2. 起 server ----
if (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue) {
    Stop-IuvServerGraceful | Out-Null
}
Start-ScheduledTask -TaskName $tn -ErrorAction Stop
Start-Sleep -Milliseconds 1200
if (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue) {
    Trace-Script "install-steps: iuv-server 已启动"
    Write-Host "iuv-server 已启动（日志：%TEMP%\iuv-server-<开机>.log）"
} else {
    Trace-Script "install-steps: iuv-server 启动后退出"
    Write-Host "警告：iuv-server 启动后退出，请查看 %TEMP%\iuv-server-<开机>.log"
}

# ---- 3. 默认配置（缺失时）----
New-DefaultConfig

# ---- 4. 重启 ctfmon，让输入法列表刷新（受限用户上下文）----
if (Restart-Ctfmon) {
    Trace-Script "install-steps: ctfmon 已重启"
} else {
    Trace-Script "install-steps: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请注销或重启一次，输入法列表会自动刷新。"
}

Trace-Script "install-steps: 完成"
exit 0
