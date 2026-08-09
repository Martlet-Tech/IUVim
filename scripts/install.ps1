# 安装 Input IME：构建检查 → 词库链 → 安装 DLL/词库 → 注册 → 重启 ctfmon。
# 需管理员权限（自动弹 UAC 提权）。用法：scripts\install.ps1
#
# 设计要点（对齐 QQ 输入法等商业安装器体验）：
# - 安装过程不杀任何进程、不要求关闭应用。
# - DLL 被占用时注册一次性 SYSTEM 计划任务延迟替换（注销/重启后生效，SYSTEM 权限不受登录限制）。
# - DLL 已注册时跳过 regsvr32。
# - ctfmon 经受限计划任务重启，避免提升 token 破坏 TSF（"只能输入英文"问题）。
#requires -Version 5.1

$ErrorActionPreference = "Stop"

# ---- 自提权：非管理员时经 UAC 重新拉起自己 ----
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
$isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $isAdmin) {
    Write-Host "需要管理员权限，正在弹出 UAC 提权窗口..."
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$($MyInvocation.MyCommand.Path)`"")
    try {
        Start-Process powershell -Verb RunAs -Wait -ArgumentList $argList
    } catch {
        Write-Host "错误：UAC 提权被取消或失败（$_）。请右键“以管理员身份运行”本脚本。"
        exit 1
    }
    exit 0
}

# ---- 脚本日志（%TEMP%\input-ime-script.log）----
# 注意：提升进程的 %TEMP% 指向系统配置目录，日志实际落点在
# C:\Windows\System32\config\systemprofile\AppData\Local\Temp\input-ime-script.log
function Trace-Script {
    param([string]$Msg)
    try { Add-Content -LiteralPath (Join-Path $env:TEMP 'input-ime-script.log') ("[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Msg) } catch {}
}

Trace-Script "install: 提升实例启动"
Write-Host "正在安装 Input IME（管理员窗口）..."

$scriptDir  = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot   = Split-Path -Parent $scriptDir
$dllSrc     = Join-Path $repoRoot "target\release\input_ime_tsf.dll"
$imedicSrc  = Join-Path $repoRoot "data\input.imedic"
$dictsDir   = Join-Path $repoRoot "data\rime-frost\cn_dicts"
$destDir    = Join-Path $env:ProgramFiles "InputIME"      # DLL：程序文件位置
$destDll    = Join-Path $destDir "input_ime_tsf.dll"
$dictDir    = Join-Path $env:LOCALAPPDATA "InputIME"      # 词库：用户级数据
$dictDest   = Join-Path $dictDir "input.imedic"
$clsidKey   = 'Registry::HKEY_CLASSES_ROOT\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey     = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'

# ---- ctfmon 重启（受限用户上下文）----
# 从提升进程直接 Start-Process 会以管理员 token 启动 ctfmon，TSF 文本服务（微软拼音等）
# 无法服务普通进程，表现为"只能输入英文"。改用一次性计划任务（/RL LIMITED /IT）在
# 用户会话、非提升上下文拉起 ctfmon，任务用完即删。
function Restart-Ctfmon {
    taskkill /f /im ctfmon.exe 2>$null | Out-Null
    Start-Sleep -Milliseconds 500
    $tn = 'InputIME-CtfmonRestart'
    $ctfmon = Join-Path $env:windir 'System32\ctfmon.exe'
    try {
        $u = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        $action = New-ScheduledTaskAction -Execute $ctfmon
        $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1)
        $principal = New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited
        $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable
        Register-ScheduledTask -TaskName $tn -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force -ErrorAction Stop | Out-Null
        Start-ScheduledTask -TaskName $tn -ErrorAction Stop
        for ($i = 0; $i -lt 6; $i++) {
            Start-Sleep -Milliseconds 500
            if (Get-Process ctfmon -ErrorAction SilentlyContinue) {
                try { Unregister-ScheduledTask -TaskName $tn -Confirm:$false -ErrorAction Stop } catch {}
                return $true
            }
        }
    } catch {
        Trace-Script ("Restart-Ctfmon: EXCEPTION " + $_)
    }
    try { Unregister-ScheduledTask -TaskName $tn -Confirm:$false -ErrorAction SilentlyContinue } catch {}
    return $false
}

# ---- PendingFileRenameOperations 双保险（无前缀 = 系统重启时由 Session Manager 执行）----
# 无前缀条目在系统启动时由 SmSs 无条件处理（Windows Update / QQ 拼音同款机制），不依赖登录会话。
# 注意：写 REG_MULTI_SZ 必须自己构造字节流——.NET 的 RegistryKey.SetValue 会丢弃数组中的
# 空字符串元素（删除条目的空目标），导致条目错位（实测踩过坑）。
function Add-PendingOp {
    param(
        [Parameter(Mandatory)][string]$Source,
        [string]$Dest
    )
    $ntSrc = "\??\$Source"
    $ntDest = if ($Dest) { "\??\$Dest" } else { "" }
    Trace-Script ("Add-PendingOp: src=" + $Source + " dest=" + $Dest)
    try {
        Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class PendingOps {
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int RegOpenKeyEx(UIntPtr hKey, string lpSubKey, int ulOptions, int samDesired, out UIntPtr phkResult);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int RegSetValueEx(UIntPtr hKey, string lpValueName, int reserved, int dwType, byte[] lpData, int cbData);
    [DllImport("advapi32.dll")]
    private static extern int RegCloseKey(UIntPtr hKey);
    private const uint HKEY_LOCAL_MACHINE = 0x80000002;
    private const int KEY_SET_VALUE = 0x0002;
    private const int REG_MULTI_SZ = 7;

    public static bool AppendMultiSz(string subKey, string valueName, string[] append) {
        object existingRaw = null;
        try {
            existingRaw = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(subKey).GetValue(valueName, null);
        } catch { }
        List<string> entries = new List<string>();
        string[] existingArr = existingRaw as string[];
        if (existingArr != null) { entries.AddRange(existingArr); }
        entries.AddRange(append);
        List<byte> ms = new List<byte>();
        foreach (string e in entries) {
            ms.AddRange(Encoding.Unicode.GetBytes(e ?? ""));
            ms.Add(0); ms.Add(0);
        }
        ms.Add(0); ms.Add(0);
        UIntPtr hk;
        if (RegOpenKeyEx(new UIntPtr(HKEY_LOCAL_MACHINE), subKey, 0, KEY_SET_VALUE, out hk) != 0) { return false; }
        try {
            return RegSetValueEx(hk, valueName, 0, REG_MULTI_SZ, ms.ToArray(), ms.Count) == 0;
        } finally { RegCloseKey(hk); }
    }
}
'@
        $ok = [PendingOps]::AppendMultiSz('SYSTEM\CurrentControlSet\Control\Session Manager', 'PendingFileRenameOperations', @($ntSrc, $ntDest))
    } catch {
        Trace-Script ("Add-PendingOp: EXCEPTION " + $_)
        return $false
    }
    Trace-Script ("Add-PendingOp: write=" + $ok)
    if (-not $ok) { return $false }
    try {
        $verify = @([string[]](Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager' -Name PendingFileRenameOperations -ErrorAction SilentlyContinue).PendingFileRenameOperations)
        $found = ($verify -contains $ntSrc)
        Trace-Script ("Add-PendingOp: verify=" + $found + " entries=[" + ($verify -join ' | ') + "]")
        return $found
    } catch {
        return $false
    }
}

# ---- 延迟操作：重启/登录触发器计划任务（清理/替换被占用文件）----
# 多触发器任务：系统重启（AtStartup）或指定用户登录（AtLogOn）时自动执行，失败则任务保留，
# 下次重启/登录自动重试，成功才自删。任务以 SYSTEM 身份运行（不依赖用户登录）。
# 与 Add-PendingOp（重启级）组成双保险。
function Register-DelayedOps {
    param(
        [string[]]$Deletes = @(),   # 要删除的路径
        [string[]]$Copies = @()     # 交替成对 @(src,dest,src,dest,...) 的复制操作
    )
    $tn = 'InputIME-DelayedOps'
    $log = Join-Path $env:TEMP 'input-ime-cleanup.log'
    $delList = ($Deletes | ForEach-Object { "'$($_.Replace("'", "''"))'" }) -join ','
    $copyList = ($Copies | ForEach-Object { "'$($_.Replace("'", "''"))'" }) -join ','
    $body = @"
`$ErrorActionPreference = 'SilentlyContinue'
`$log = '$log'
`$deletes = @($delList)
`$copies = @($copyList)
`$ok = `$false
for (`$i = 0; `$i -lt 12; `$i++) {
    for (`$j = 0; `$j -lt `$copies.Count; `$j += 2) {
        Copy-Item -LiteralPath `$copies[`$j] -Destination `$copies[`$j+1] -Force
    }
    for (`$k = 0; `$k -lt `$deletes.Count; `$k++) {
        Remove-Item -LiteralPath `$deletes[`$k] -Recurse -Force
    }
    `$left = @(`$deletes | Where-Object { Test-Path -LiteralPath `$_ })
    if (`$left.Count -eq 0) { `$ok = `$true; break }
    Start-Sleep -Seconds 5
}
"delayed-ops `$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') ok=`$ok left=`$(`$left -join ';')" | Add-Content -LiteralPath `$log
if (`$ok) { Unregister-ScheduledTask -TaskName `$tn -Confirm:`$false -ErrorAction Stop }
"@
    $b64 = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($body))
    Trace-Script ("Register-DelayedOps: deletes=[" + ($Deletes -join ';') + "] copies=[" + ($Copies -join ';') + "]")
    try {
        $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ("-NoProfile -ExecutionPolicy Bypass -EncodedCommand " + $b64)
        $triggerBoot = New-ScheduledTaskTrigger -AtStartup
        $triggerLogon = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
        $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
        $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
        Register-ScheduledTask -TaskName $tn -Action $action -Trigger @($triggerBoot, $triggerLogon) -Principal $principal -Settings $settings -Force -ErrorAction Stop | Out-Null
    } catch {
        Trace-Script ("Register-DelayedOps: EXCEPTION " + $_)
        return $false
    }
    Trace-Script "Register-DelayedOps: 任务已注册 $tn (AtStartup + AtLogOn)"
    return $true
}

# ---- 1. 构建检查 ----
if (-not (Test-Path $dllSrc)) {
    Trace-Script "install: 错误，构建产物缺失 $dllSrc"
    Write-Host "错误：未找到 $dllSrc"
    Write-Host "请先执行：cargo build -p ime-tsf --release"
    exit 1
}
Trace-Script "install: 构建产物 OK"
Write-Host "构建产物 OK：$dllSrc"

# ---- 2. 词库链：imedic 缺失时自动下载 + 编译 ----
if (-not (Test-Path $imedicSrc)) {
    Trace-Script "install: 词库缺失，进入下载/编译流程"
    if (-not (Test-Path $dictsDir) -or ((Get-ChildItem $dictsDir -Filter *.dict.yaml).Count -eq 0)) {
        Write-Host "词库源缺失，正在下载（scripts\download-dict.ps1）..."
        Push-Location $repoRoot
        try { & (Join-Path $scriptDir "download-dict.ps1") }
        finally { Pop-Location }
    }
    Write-Host "正在编译词库（dictc）..."
    Push-Location $repoRoot
    try {
        $yamlFiles = (Get-ChildItem $dictsDir -Filter *.dict.yaml | ForEach-Object { $_.FullName })
        if ($yamlFiles.Count -eq 0) { throw "词库目录为空：$dictsDir" }
        cargo run -p ime-data --bin dictc -- $imedicSrc $yamlFiles
        if ($LASTEXITCODE -ne 0) { throw "dictc 编译失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    if (-not (Test-Path $imedicSrc)) { throw "编译完成但未找到 $imedicSrc" }
}
Trace-Script "install: 词库 OK"
Write-Host "词库 OK：$imedicSrc"

# ---- 3. 安装 DLL（被占用时排队替换，注销/重启后生效）----
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$queuedReplace = $false
try {
    Copy-Item $dllSrc $destDll -Force -ErrorAction Stop
    Trace-Script "install: DLL 复制成功 $destDll"
    Write-Host "已安装 DLL：$destDll"
} catch {
    # 目标被占用：复制到临时名，注册一次性 SYSTEM 任务做延迟替换（注销/重启后自动生效）。
    $tmp = Join-Path $destDir "input_ime_tsf.new.dll"
    Copy-Item $dllSrc $tmp -Force
    Trace-Script "install: DLL 被占用，延迟替换 $tmp -> $destDll"
    Add-PendingOp -Source $tmp -Dest $destDll
    if (Register-DelayedOps -Copies @($tmp, $destDll) -Deletes @($tmp)) {
        $queuedReplace = $true
        Write-Host "DLL 正被占用（无需关闭应用），已安排延迟替换，注销或重启后生效：$destDll"
    } else {
        throw "DLL 被占用且延迟替换任务注册失败，请重启后重新运行本脚本"
    }
}

# ---- 4. 安装词库（用户级数据，%LOCALAPPDATA% 下不锁定）----
New-Item -ItemType Directory -Force -Path $dictDir | Out-Null
Copy-Item $imedicSrc $dictDest -Force
Trace-Script "install: 词库复制 $dictDest"
Write-Host "已安装词库：$dictDest"

# ---- 5. 注册（仅未注册时；DLL 路径不变，升级无需重注册）----
if (-not (Test-Path $clsidKey) -or -not (Test-Path $tipKey)) {
    Trace-Script "install: 开始 regsvr32 $destDll"
    Write-Host "正在注册 COM/TSF 服务..."
    & "$env:windir\System32\regsvr32.exe" /s $destDll
    Start-Sleep -Seconds 1
    Trace-Script ("install: regsvr32 返回，CLSID=" + (Test-Path $clsidKey) + " TIP=" + (Test-Path $tipKey))
    # 不依赖 regsvr32 退出码（$LASTEXITCODE 可能为 $null）；以注册表是否写入为准。
    if (-not (Test-Path $clsidKey) -or -not (Test-Path $tipKey)) {
        Write-Host "错误：注册失败（CLSID=$(Test-Path $clsidKey) TIP=$(Test-Path $tipKey)）。日志见 %TEMP%\input-ime-tsf.log"
        exit 1
    }
} else {
    Trace-Script "install: 已注册，跳过 regsvr32"
    Write-Host "已注册（跳过 regsvr32）。"
}

# ---- 6. 重启 ctfmon（受限用户上下文，避免提升 token 破坏输入法）----
if (-not (Restart-Ctfmon)) {
    Trace-Script "install: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请手动重启（任务管理器结束 ctfmon.exe 后，新建任务运行 ctfmon.exe），或注销/重启后生效。"
} else {
    Trace-Script "install: ctfmon 已重启"
}

Write-Host ""
if ($queuedReplace) { Write-Host "注意：新版本 DLL 将在注销或重启后生效。" }
Trace-Script "install: 安装完成"
Write-Host "安装完成。下一步：Windows 设置 → 时间和语言 → 语言 → 中文 → 键盘 → 切换到 'Input IME'"
Write-Host "安装完成。下一步：Windows 设置 → 时间和语言 → 语言 → 中文 → 键盘 → 切换到 'Input IME'"
