# Input IME 安装/卸载公共库（install.ps1 / uninstall.ps1 dot-source 共享）。
# 只定义函数，无顶层副作用。用法：. (Join-Path $PSScriptRoot 'ime-common.ps1')
#requires -Version 5.1

$ErrorActionPreference = "Stop"

# ---- 自提权：非管理员时经 UAC 重新拉起自己，然后退出 ----
# $ScriptPath 必须由调用方在脚本顶层传入（$PSCommandPath）；函数内取不到脚本级变量。
# $PassArgs：可选，UAC 重拉时附加到命令行的参数（如 -SkipBuild），保持调用语义不变。
function Exit-IfNotAdmin {
    param(
        [string]$ScriptPath,
        [string[]]$PassArgs = @()
    )
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    $isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if ($isAdmin) { return }
    if ([string]::IsNullOrEmpty($ScriptPath)) {
        Write-Host "错误：无法确定脚本路径（UAC 提权无法执行）。请右键“以管理员身份运行”本脚本。"
        exit 1
    }
    Write-Host "需要管理员权限，正在弹出 UAC 提权窗口..."
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$ScriptPath`"") + $PassArgs
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

# ---- DLL 热替换（dev-deploy 用，零杀进程）----
# Windows 加载 DLL 时授予 FILE_SHARE_DELETE：已加载的 DLL 可以改名但不能覆盖。
# 策略：直接复制（未锁）→ 改名 .old + 原位复制（已锁）→ 改名也失败则报告持锁进程（不强杀）。
# .old 的延迟清理复用双保险（Add-PendingOp 重启删 + Register-DelayedOps 注销删）。
function Replace-InUseDll {
    param(
        [Parameter(Mandatory)][string]$Src,
        [Parameter(Mandatory)][string]$Dest,
        [string]$OldSuffix = ".old"
    )
    # 1) 快速路径：未锁直接覆盖
    try {
        Copy-Item $Src $Dest -Force -ErrorAction Stop
        Trace-Script "Replace-InUseDll: 直接复制成功 $Dest"
        return @{ Ok = $true; Renamed = $false }
    } catch {
        Trace-Script "Replace-InUseDll: 直接复制失败（被占用），尝试改名替换"
    }
    # 2) rename-then-copy：改名旧 DLL 让出原名，再原位写入新 DLL
    # 旧 .old 若已存在（上一轮热部署遗留，尚未到注销/重启清理），追加时间戳后缀避免冲突。
    $oldPath = "$Dest$OldSuffix"
    if (Test-Path -LiteralPath $oldPath) {
        $oldPath = "$Dest$OldSuffix.$([DateTime]::Now.ToString('yyyyMMddHHmmss'))"
    }
    try {
        Move-Item -LiteralPath $Dest -Destination $oldPath -Force -ErrorAction Stop
        Copy-Item $Src $Dest -Force -ErrorAction Stop
        Trace-Script "Replace-InUseDll: 改名替换成功 $oldPath <- $Dest"
        # 双保险安排 .old 延迟清理（老进程仍持旧映射，注销/重启后删除）
        $p1 = Add-PendingOp -Source $oldPath
        $p2 = Register-DelayedOps -Deletes @($oldPath)
        if (-not ($p1 -or $p2)) {
            Write-Host "警告：$oldPath 延迟清理登记失败，请注销/重启后手动删除。"
        }
        return @{ Ok = $true; Renamed = $true; OldPath = $oldPath }
    } catch {
        Trace-Script ("Replace-InUseDll: 改名替换失败 " + $_)
        # 3) 兜底：报告持锁进程，绝不强杀
        $holders = Get-FileHolders -Path $Dest
        Write-Host "错误：DLL 无法替换，被以下进程占用："
        $holders | ForEach-Object { Write-Host "  $_" }
        Write-Host "请关闭这些进程后重跑，或改用 scripts\install.ps1（延迟替换，注销/重启后生效）。"
        return @{ Ok = $false }
    }
}

# 枚举占用文件的进程（Restart Manager API：RmStartSession → RmRegisterResources → RmGetList）。
# 仅用于报告，不关停任何进程。返回进程名字符串数组。
function Get-FileHolders {
    param([Parameter(Mandatory)][string]$Path)
    try {
        Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class FileHolders {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct RM_UNIQUE_PROCESS { public int dwProcessId; public System.Runtime.InteropServices.ComTypes.FILETIME ProcessStartTime; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct RM_PROCESS_INFO {
        public RM_UNIQUE_PROCESS Process;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strAppName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string strServiceShortName;
        public int ApplicationType;
        public uint AppStatus;
        public uint TSSessionId;
        [MarshalAs(UnmanagedType.Bool)] public bool bRestartable;
    }
    [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
    static extern int RmStartSession(out uint pSessionHandle, int dwSessionFlags, StringBuilder strSessionKey);
    [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
    static extern int RmRegisterResources(uint dwSessionHandle, uint nFiles, string[] rgsFilenames, uint nServices, string[] rgsServiceNames, uint nApplications, RM_UNIQUE_PROCESS[] rgApplications);
    [DllImport("rstrtmgr.dll")]
    static extern int RmGetList(uint dwSessionHandle, out uint pnProcInfoNeeded, ref uint pnProcInfo, [In, Out] RM_PROCESS_INFO[] rgAffectedApps, ref uint lpdwRebootReasons);
    [DllImport("rstrtmgr.dll")]
    static extern int RmEndSession(uint dwSessionHandle);

    public static string[] GetHolders(string path) {
        List<string> names = new List<string>();
        uint session;
        StringBuilder key = new StringBuilder(256);
        if (RmStartSession(out session, 0, key) != 0) return names.ToArray();
        try {
            string[] files = new string[] { path };
            RmRegisterResources(session, 1, files, 0, null, 0, null);
            uint needed = 0, count = 0, reboot = 0;
            RM_PROCESS_INFO[] procs = new RM_PROCESS_INFO[16];
            count = 16;
            int ret = RmGetList(session, out needed, ref count, procs, ref reboot);
            if (ret == 234 && needed > count) { // ERROR_MORE_DATA
                procs = new RM_PROCESS_INFO[needed];
                count = needed;
                ret = RmGetList(session, out needed, ref count, procs, ref reboot);
            }
            if (ret != 0) return names.ToArray();
            for (int i = 0; i < count; i++) {
                names.Add(procs[i].strAppName + " (pid=" + procs[i].Process.dwProcessId + ")");
            }
        } finally { RmEndSession(session); }
        return names.ToArray();
    }
}
'@
        return @([FileHolders]::GetHolders($Path))
    } catch {
        Trace-Script ("Get-FileHolders: EXCEPTION " + $_)
        return @("（无法枚举占用进程：" + $_.Exception.Message + "）")
    }
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
