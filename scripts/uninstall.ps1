# 卸载 Input IME：删注册表键 → 重启 ctfmon → 删文件 → 自检。
# 需管理员权限（自动弹 UAC 提权）。用法：scripts\uninstall.ps1
#
# 设计要点：
# - 不调用 regsvr32 /u（会加载 DLL，被占用时挂起）；注册键直接删，效果相同。
# - 不杀 explorer、不要求关闭应用：DLL 被占用只影响文件删除，不影响注册表与列表刷新。
# - 被占用的残留文件注册一次性 SYSTEM 计划任务延迟清理（注销/重启后自动执行，不依赖用户登录；
#   PendingFileRenameOperations 会话前缀实测不可靠，弃用）。
# - ctfmon 重启后，托盘语言栏中的 "Input IME" 即从列表消失。
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

Trace-Script "uninstall: 提升实例启动"
Write-Host "正在卸载 Input IME（管理员窗口）..."

$destDir = Join-Path $env:ProgramFiles "InputIME"
$clsid = '{C69735F1-BAB1-458B-89FC-099ABA877ECB}'

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

# 本输入法注册的键（与 crates/ime-tsf/src/registration.rs 对应）：
# 1) HKCR\CLSID\{GUID}      —— COM 类注册（DllRegisterServer 写）
# 2) HKLM\...\CTF\TIP\{GUID} —— TSF 文本服务注册（ITfInputProcessorProfiles 写）
# 3) HKCU\...\CTF\TIP\{GUID} —— 无管理员权限时 TSF 注册可能落到用户级
$keys = @(
    "Registry::HKEY_CLASSES_ROOT\CLSID\$clsid",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\$clsid",
    "Registry::HKEY_CURRENT_USER\SOFTWARE\Microsoft\CTF\TIP\$clsid"
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

# ---- 2. 重启 ctfmon：此时注册表已无本输入法，ctfmon 不再加载 DLL（也刷新托盘列表）----
if (-not (Restart-Ctfmon)) {
    Trace-Script "uninstall: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请手动重启（任务管理器结束 ctfmon.exe 后，新建任务运行 ctfmon.exe），或注销/重启后输入法列表自动刷新。"
} else {
    Trace-Script "uninstall: ctfmon 已重启"
}

# ---- 3. 删除安装文件与用户数据 ----
# 未锁定的文件直接删；有残留（被锁）时整体交给一次性 SYSTEM 任务延迟清理
# （注销/重启后自动执行，无需用户登录）。
$delayed = $false
if (Test-Path $destDir) {
    $locked = @()
    Get-ChildItem -LiteralPath $destDir -Force -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.PSIsContainer) { return }
        $p = $_.FullName
        try {
            Remove-Item -LiteralPath $p -Force -ErrorAction Stop
        } catch {
            $locked += $p
        }
    }
    if ($locked.Count -gt 0) { Trace-Script ("uninstall: 发现锁定文件 " + ($locked -join ', ')) }
    # 目录：未锁文件删完后若已空则删除；否则保留空壳（延迟清理会连壳删掉）。
    try { Remove-Item -LiteralPath $destDir -Recurse -Force -ErrorAction Stop } catch {}
    if ($locked.Count -eq 0 -and -not (Test-Path $destDir)) {
        Trace-Script "uninstall: 安装目录已删除"
        Write-Host "已删除安装目录：$destDir"
    } else {
        $delayed = $true
    }
}

$userData = Join-Path $env:LOCALAPPDATA "InputIME"
if (Test-Path $userData) {
    try {
        Remove-Item -LiteralPath $userData -Recurse -Force -ErrorAction Stop
        Trace-Script "uninstall: 删除用户数据 $userData"
        Write-Host "已删除用户数据（词库）：$userData"
    } catch {
        $delayed = $true
    }
}

# 旧版本残留（早期安装在 %ProgramData%\InputIME）：顺带清理。
$legacyDir = Join-Path $env:ProgramData "InputIME"
if (Test-Path $legacyDir) {
    try { Remove-Item -LiteralPath $legacyDir -Recurse -Force -ErrorAction Stop } catch {}
    if (Test-Path $legacyDir) {
        Trace-Script "uninstall: 旧版残留未删净 $legacyDir"
        $delayed = $true
    } else {
        Write-Host "已清理旧版残留：$legacyDir"
    }
}

if ($delayed) {
    $paths = @($destDir, $userData, $legacyDir | Where-Object { Test-Path -LiteralPath $_ })
    Trace-Script ("uninstall: 残留路径待延迟清理 [" + ($paths -join ';') + "]")
    # 双保险：写无前缀 PendingFileRenameOperations（系统重启时由 Session Manager 无条件执行）
    foreach ($p in $paths) { Add-PendingOp -Source $p }
    if (Register-DelayedOps -Deletes $paths) {
        Write-Host "以下残留正被占用，已安排自动清理（注销或重启后生效，无需手动操作）："
        $paths | ForEach-Object { Write-Host "  $_" }
    } else {
        Write-Host "警告：自动清理任务注册失败，请注销/重启后手动删除：$($paths -join ', ')"
    }
}

# ---- 4. 自检 ----
$left = @($keys | Where-Object { Test-Path $_ })
if ($left.Count -gt 0) {
    Trace-Script "uninstall: 残留注册表键 $($left -join ';')"
    Write-Host "警告：仍有残留注册表键：$($left -join ', ')"
    exit 1
}
if ($delayed) {
    if (Get-ScheduledTask -TaskName InputIME-DelayedOps -ErrorAction SilentlyContinue) {
        Trace-Script "uninstall: 完成（延迟清理任务已注册）"
        Write-Host ""
        Write-Host "卸载完成。残留文件将在注销或重启后自动清理。"
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
