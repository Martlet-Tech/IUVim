# Input IME 安装/卸载公共库（install.ps1 / uninstall.ps1 dot-source 共享）。
# 只定义函数，无顶层副作用。用法：. (Join-Path $PSScriptRoot 'ime-common.ps1')
#requires -Version 5.1

$ErrorActionPreference = "Stop"

# ---- 自提权：非管理员时经 UAC 重新拉起自己，然后退出 ----
# $ScriptPath 必须由调用方在脚本顶层传入（$PSCommandPath）；函数内取不到脚本级变量。
function Exit-IfNotAdmin {
    param([string]$ScriptPath)
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    $isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if ($isAdmin) { return }
    if ([string]::IsNullOrEmpty($ScriptPath)) {
        Write-Host "错误：无法确定脚本路径（UAC 提权无法执行）。请右键“以管理员身份运行”本脚本。"
        exit 1
    }
    Write-Host "需要管理员权限，正在弹出 UAC 提权窗口..."
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$ScriptPath`"")
    try {
        Start-Process powershell -Verb RunAs -Wait -ArgumentList $argList
    } catch {
        Write-Host "错误：UAC 提权被取消或失败（$_）。请右键“以管理员身份运行”本脚本。"
        exit 1
    }
    exit 0
}

# ---- 脚本日志（用户 %TEMP%\input-ime-script.log）----
# 提升进程继承发起用户的环境变量，%TEMP% 仍为用户 TEMP（实测验证）。
function Trace-Script {
    param([string]$Msg)
    try { Add-Content -LiteralPath (Join-Path $env:TEMP 'input-ime-script.log') ("[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Msg) } catch {}
}

# ---- ctfmon 重启（受限用户上下文）----
# 提升进程直接启动 ctfmon 会带管理员 token，TSF 文本服务无法服务普通进程（"只能输入英文"）。
# 改用一次性计划任务（受限 token、交互式）在用户会话拉起 ctfmon，任务用完即删。
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

# ---- 延迟清理双保险 ----
# A. Add-PendingOp：写无前缀 PendingFileRenameOperations 条目，系统重启时由 Session Manager
#    无条件处理（Windows Update 同款机制），不依赖登录会话。
# B. Register-DelayedOps：注册多触发器计划任务（AtStartup + AtLogOn，SYSTEM 身份）在
#    重启/登录时执行删除/替换；失败则任务保留、下次自动重试，成功才自删。
#
# 注意：写 REG_MULTI_SZ 必须自己构造字节流——RegistryKey.SetValue 会丢弃数组中的空字符串
# 元素（删除条目的空目标），导致条目错位（实测踩过坑）。
# 去重：AppendMultiSz 按 src 整对去重——写入时折叠已存在的重复对（旧版无去重写入的脏条目
# 也会被折叠），防止条目无限累积。
function Ensure-PendingOpsType {
    if ('PendingOps' -as [type]) { return }
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

    private static string[] ReadMultiSz(string subKey, string valueName) {
        try {
            return Microsoft.Win32.Registry.LocalMachine.OpenSubKey(subKey).GetValue(valueName, null) as string[];
        } catch {
            return null;
        }
    }

    private static bool WriteMultiSz(string subKey, string valueName, List<string> entries) {
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

    // 按 src 整对去重：先折叠已有重复对，再跳过与已有 src 相同的追加对。
    public static bool AppendMultiSz(string subKey, string valueName, string[] append) {
        List<string> entries = new List<string>();
        HashSet<string> seen = new HashSet<string>();
        string[] existingArr = ReadMultiSz(subKey, valueName);
        if (existingArr != null) {
            for (int i = 0; i < existingArr.Length; i += 2) {
                string src = existingArr[i] ?? "";
                if (src.Length > 0 && !seen.Add(src)) { continue; }
                entries.Add(src);
                if (i + 1 < existingArr.Length) { entries.Add(existingArr[i + 1] ?? ""); }
            }
        }
        for (int i = 0; i < append.Length; i += 2) {
            string src = append[i] ?? "";
            if (src.Length > 0 && !seen.Add(src)) { continue; }
            entries.Add(src);
            if (i + 1 < append.Length) { entries.Add(append[i + 1] ?? ""); }
        }
        return WriteMultiSz(subKey, valueName, entries);
    }

    // 移除 src 命中的整对条目（用于安装前清理指向安装路径的陈旧 pending op）。
    public static bool RemoveSrc(string subKey, string valueName, string[] remove) {
        List<string> entries = new List<string>();
        string[] existingArr = ReadMultiSz(subKey, valueName);
        if (existingArr != null) {
            for (int i = 0; i < existingArr.Length; i += 2) {
                string src = existingArr[i] ?? "";
                if (Array.IndexOf(remove, src) >= 0) { continue; }
                entries.Add(src);
                if (i + 1 < existingArr.Length) { entries.Add(existingArr[i + 1] ?? ""); }
            }
        }
        return WriteMultiSz(subKey, valueName, entries);
    }
}
'@
}

function Add-PendingOp {
    param(
        [Parameter(Mandatory)][string]$Source,
        [string]$Dest
    )
    $ntSrc = "\??\$Source"
    $ntDest = if ($Dest) { "\??\$Dest" } else { "" }
    Trace-Script ("Add-PendingOp: src=" + $Source + " dest=" + $Dest)
    try {
        Ensure-PendingOpsType
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

# 安装前防御：清除指向指定路径的陈旧 pending op（"重启删目录"条目会误删新装的目录）。
function Clear-PendingOp {
    param(
        [Parameter(Mandatory)][string]$Source
    )
    $ntSrc = "\??\$Source"
    Trace-Script ("Clear-PendingOp: src=" + $Source)
    try {
        Ensure-PendingOpsType
        return [PendingOps]::RemoveSrc('SYSTEM\CurrentControlSet\Control\Session Manager', 'PendingFileRenameOperations', @($ntSrc))
    } catch {
        Trace-Script ("Clear-PendingOp: EXCEPTION " + $_)
        return $false
    }
}

function Register-DelayedOps {
    param(
        [string[]]$Deletes = @(),   # 要删除的路径
        [string[]]$Copies = @()     # 交替成对 @(src,dest,src,dest,...) 的复制操作
    )
    $tn = 'InputIME-DelayedOps'
    # body 内 log 路径在此（提升进程，用户 TEMP）算成绝对路径再拼入，不依赖任务运行环境。
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
    # 先清同名旧任务再注册：-Force 覆盖会保留旧 SecurityDescriptor（普通权限将查不到任务）。
    try { Unregister-ScheduledTask -TaskName $tn -Confirm:$false -ErrorAction SilentlyContinue } catch {}
    try {
        $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ("-NoProfile -ExecutionPolicy Bypass -EncodedCommand " + $b64)
        $triggerBoot = New-ScheduledTaskTrigger -AtStartup
        $triggerLogon = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
        $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
        $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
        Register-ScheduledTask -TaskName $tn -Action $action -Trigger @($triggerBoot, $triggerLogon) -Principal $principal -Settings $settings -Force -ErrorAction Stop | Out-Null
        if (-not (Get-ScheduledTask -TaskName $tn -ErrorAction SilentlyContinue)) { throw "注册后验证失败" }
    } catch {
        Trace-Script ("Register-DelayedOps: EXCEPTION " + $_)
        return $false
    }
    Trace-Script "Register-DelayedOps: 任务已注册 $tn (AtStartup + AtLogOn)"
    return $true
}
