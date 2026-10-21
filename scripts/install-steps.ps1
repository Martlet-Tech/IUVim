# 安装包内的「装后动作」（61 号）：替换二进制 → 注册 COM/TSF → 计划任务 → 起 server → 默认配置 → 重启 ctfmon。
#
# ## 为什么二进制不直接由 Inno 拷到 {app}
#
# 输入法的硬约束：**TSF DLL 被全系统运行中的进程映射**（explorer / ctfmon / 每个
# 加载过输入法的应用都持着 image section）。Inno 的 [Files] 覆盖已存在文件时走的是
# DeleteFile + 重建，此时必然 sharing violation → 弹「DeleteFile 失败：错误代码 5。
# 拒绝访问」，重试永远过不去（2026-10-21 用户真机覆盖安装卡死就是这个）。
#
# 仓库脚本早就解过这题（`iuv-common.ps1` 的 `Replace-InUseFile`）：已加载的 DLL 是
# 以 FILE_SHARE_DELETE 映射的，**可以改名**——把旧文件改名成 `.old` 让出原名，再在
# 原位写新文件。老进程继续持旧映射，新进程加载新 DLL，零杀进程、免注销。
#
# 所以安装包把新文件放 `{app}\payload\`，由本脚本按热替换规则落到 `{app}`。
#
# 用法（Inno [Run] 调用，已提权）：powershell -File install-steps.ps1
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')

$p = Get-IuvPaths
$appDir = $p.DestDir
$payload = Join-Path $appDir 'payload'
Trace-Script "install-steps: 开始（app=$appDir payload=$payload）"

if (-not (Test-Path $p.ServerDst) -and -not (Test-Path $payload)) {
    Trace-Script "install-steps: 既没有 $($p.ServerDst) 也没有 $payload"
    Write-Host "错误：安装目录不完整（既无 iuv-server.exe 也无 payload/）"
    exit 1
}

# ---- 1. 二进制落地（热替换，允许覆盖正在运行的旧版）----
# payload 缺失 = 重复运行本脚本（文件已落地过一次），跳过而不报错。
if (Test-Path $payload) {
    $pairs = @(
        @{ Src = 'iuv_tsf.dll';     Dst = $p.DestDll },
        @{ Src = 'iuv_tsf_x86.dll'; Dst = $p.DestDll32 },
        @{ Src = 'iuv-server.exe';  Dst = $p.ServerDst }
    )
    foreach ($pair in $pairs) {
        $src = Join-Path $payload $pair.Src
        if (-not (Test-Path $src)) {
            Trace-Script "install-steps: payload 缺少 $($pair.Src)"
            Write-Host "错误：安装包不完整，缺少 $($pair.Src)"
            exit 1
        }
        $r = Replace-InUseFile -Src $src -Dest $pair.Dst
        if (-not $r.Ok) {
            # 改名也失败 = 连 FILE_SHARE_DELETE 都没给（或权限问题）：不再往下走，
            # 否则会注册一个半旧半新的组合。
            Trace-Script "install-steps: 替换失败 $($pair.Dst)"
            Write-Host "错误：无法替换 $($pair.Dst)（被占用且不允许改名）。"
            Write-Host "请注销或重启后重新运行安装包。"
            exit 1
        }
        if ($r.Renamed) {
            Write-Host "已热替换 $($pair.Src)（旧文件改名 $([System.IO.Path]::GetFileName($r.OldPath))，注销/重启后自动清理）"
        } else {
            Write-Host "已安装 $($pair.Src)"
        }
    }
    # payload 用完即清（Inno 的卸载日志会跳过已不存在的文件）
    Remove-Item $payload -Recurse -Force -ErrorAction SilentlyContinue
} else {
    Trace-Script "install-steps: 无 payload（重复运行），跳过文件替换"
}

# ---- 2. 注册 COM/TSF（双架构；32 位必须用 SysWOW64 的 regsvr32 才落 WoW64 视图）----
# 必须在第 1 步之后：老进程持旧映射不影响注册——regsvr32 是**新进程**，加载的是新文件。
foreach ($pair in @(
    @{ Exe = "$env:windir\System32\regsvr32.exe"; Dll = $p.DestDll },
    @{ Exe = "$env:windir\SysWOW64\regsvr32.exe"; Dll = $p.DestDll32 }
)) {
    if (-not (Test-Path $pair.Dll)) {
        Trace-Script "install-steps: 缺少 DLL $($pair.Dll)"
        Write-Host "错误：缺少 $($pair.Dll)"
        exit 1
    }
    & $pair.Exe /s $pair.Dll
    # 不依赖 regsvr32 退出码（被占用时会加载 DLL 且 $LASTEXITCODE 可能为 $null）；
    # 注册是否成功以注册表为准，与 install.ps1 同判据。
    Start-Sleep -Milliseconds 300
}
$clsid = '{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
foreach ($key in @(
    "Registry::HKEY_CLASSES_ROOT\CLSID\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\WOW6432Node\CLSID\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\CTF\TIP\$clsid"
)) {
    if (-not (Test-Path $key)) {
        Trace-Script "install-steps: 注册缺失 $key"
        Write-Host "错误：注册表项缺失 $key（regsvr32 未生效）"
        exit 1
    }
}
Trace-Script "install-steps: COM/TSF 注册完成（双架构）"

# ---- 3. 计划任务：登录自启 + 周期自愈（与 Deploy-IuvServer 同参数）----
# 单独重写而不是调 Deploy-IuvServer：那条路径会先从 target-server 复制 exe，而安装包里
# exe 已落地。参数必须保持同步——改一处要改两处，两处都带这段注释。
# 崩溃自愈用「1 分钟重复触发器 + IgnoreNew」而非 RestartOnFailure（后者本机 4 轮
# 探针实测从不触发），详见 iuv-common.ps1 同名函数的注释与 61 号 §3.3。
$tn = 'Iuv-ServerStart'
$u = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $p.ServerDst -WorkingDirectory $appDir
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

# ---- 4. 起 server ----
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

# ---- 5. 默认配置（缺失时；已有配置不动——覆盖安装不能丢用户设置）----
New-DefaultConfig

# ---- 6. 重启 ctfmon，让输入法列表刷新（受限用户上下文）----
if (Restart-Ctfmon) {
    Trace-Script "install-steps: ctfmon 已重启"
    Write-Host "输入法已注册并刷新（现在可以在系统设置里切到「IUV 输入法」）"
} else {
    Trace-Script "install-steps: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请注销或重启一次，输入法列表会自动刷新。"
}

Trace-Script "install-steps: 完成"
exit 0
