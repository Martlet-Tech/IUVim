# iuv 输入法 部署/卸载公共库（install.ps1 / dev-deploy.ps1 / uninstall.ps1 / build.ps1
# dot-source 共享）。只定义函数，无顶层副作用。用法：. (Join-Path $PSScriptRoot 'iuv-common.ps1')
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

# ---- 脚本日志（用户 %TEMP%\iuv-script.log）----
# 提升进程继承发起用户的环境变量，%TEMP% 仍为用户 TEMP（实测验证）。
function Trace-Script {
    param([string]$Msg)
    try { Add-Content -LiteralPath (Join-Path $env:TEMP 'iuv-script.log') ("[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Msg) } catch {}
}

# ---- iuv-server 优雅停机（哨兵文件 + 强杀兜底）----
# server 监视 %LOCALAPPDATA%\iuv\server.stop（config_watch 500ms 周期），检测到
# 即广播 Push::Shutdown（客户端立即透明放行）后自行退出。等待至多 5s，仍未退出
# （如旧版 server 无哨兵逻辑）回退 Stop-Process -Force。返回 $true = 进程已停止。
function Stop-IuvServerGraceful {
    $stopFile = Join-Path $env:LOCALAPPDATA 'iuv\server.stop'
    $running = Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue
    if (-not $running) { return $true }
    Write-Host "请求 iuv-server 优雅停机（哨兵 $stopFile）..."
    Trace-Script "Stop-IuvServerGraceful: 写哨兵（PID=$($running.Id -join ',')）"
    New-Item -ItemType File -Force -Path $stopFile | Out-Null
    for ($i = 0; $i -lt 10; $i++) {
        Start-Sleep -Milliseconds 500
        if (-not (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue)) {
            Remove-Item $stopFile -Force -ErrorAction SilentlyContinue
            Write-Host "iuv-server 已优雅退出"
            return $true
        }
    }
    Write-Host "警告：优雅停机超时（5s），强杀兜底（旧版 server 无哨兵逻辑？）"
    Trace-Script "Stop-IuvServerGraceful: 超时强杀兜底"
    Stop-Process -Name 'iuv-server' -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 300
    Remove-Item $stopFile -Force -ErrorAction SilentlyContinue
    return -not (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue)
}

# ---- ctfmon 重启（受限用户上下文）----
# 提升进程直接启动 ctfmon 会带管理员 token，TSF 文本服务无法服务普通进程（"只能输入英文"）。
# 改用一次性计划任务（受限 token、交互式）在用户会话拉起 ctfmon，任务用完即删。
function Restart-Ctfmon {
    taskkill /f /im ctfmon.exe 2>$null | Out-Null
    Start-Sleep -Milliseconds 500
    $tn = 'Iuv-CtfmonRestart'
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

# ---- 文件热替换（dev-deploy/install 用，零杀进程）----
# DLL 与词库通用：已加载 DLL 授予 FILE_SHARE_DELETE 可改名；词库 mmap 同样声明 FILE_SHARE_DELETE
# （mmap.rs）可改名但截断写被拒（ERROR_USER_MAPPED_FILE）。改名替换让被锁文件让出原名，
# 老进程持旧映射（.old）、新进程加载新文件——两个场景同一策略。
# 流程：直接复制（未锁）→ 改名 .old + 原位复制（已锁）→ 改名也失败则报告持锁进程（不强杀）。
# .old 的延迟清理复用双保险（Add-PendingOp 重启删 + Register-DelayedOps 注销删）。
function Replace-InUseFile {
    param(
        [Parameter(Mandatory)][string]$Src,
        [Parameter(Mandatory)][string]$Dest,
        [string]$OldSuffix = ".old",
        [switch]$WarnOnly   # 失败只警告返回 Ok=$false（词库等非关键产物），不报错退出
    )
    # 1) 快速路径：未锁直接覆盖
    try {
        Copy-Item $Src $Dest -Force -ErrorAction Stop
        Trace-Script "Replace-InUseFile: 直接复制成功 $Dest"
        return @{ Ok = $true; Renamed = $false }
    } catch {
        Trace-Script "Replace-InUseFile: 直接复制失败（被占用），尝试改名替换"
    }
    # 2) rename-then-copy：改名旧文件让出原名，再原位写入新文件
    # 旧 .old 若已存在（上一轮热部署遗留，尚未到注销/重启清理），追加时间戳后缀避免冲突。
    $oldPath = "$Dest$OldSuffix"
    if (Test-Path -LiteralPath $oldPath) {
        $oldPath = "$Dest$OldSuffix.$([DateTime]::Now.ToString('yyyyMMddHHmmss'))"
    }
    try {
        Move-Item -LiteralPath $Dest -Destination $oldPath -Force -ErrorAction Stop
        Copy-Item $Src $Dest -Force -ErrorAction Stop
        Trace-Script "Replace-InUseFile: 改名替换成功 $oldPath <- $Dest"
        # 双保险安排 .old 延迟清理（老进程仍持旧映射，注销/重启后删除）
        $p1 = Add-PendingOp -Source $oldPath
        $p2 = Register-DelayedOps -Deletes @($oldPath)
        if (-not ($p1 -or $p2)) {
            Write-Host "警告：$oldPath 延迟清理登记失败，请注销/重启后手动删除。"
        }
        return @{ Ok = $true; Renamed = $true; OldPath = $oldPath }
    } catch {
        Trace-Script ("Replace-InUseFile: 改名替换失败 " + $_)
        # 3) 兜底：报告持锁进程，绝不强杀
        $holders = Get-FileHolders -Path $Dest
        if ($WarnOnly) {
            Write-Host "警告：$Dest 无法替换，本次跳过（被以下进程占用）："
            $holders | ForEach-Object { Write-Host "  $_" }
            Write-Host "请关闭这些进程后重跑，或注销/重启后生效。"
        } else {
            Write-Host "错误：$Dest 无法替换，被以下进程占用："
            $holders | ForEach-Object { Write-Host "  $_" }
            Write-Host "请关闭这些进程后重跑，或改用 scripts\install.ps1（延迟替换，注销/重启后生效）。"
        }
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
    $tn = 'Iuv-DelayedOps'
    # body 内 log 路径在此（提升进程，用户 TEMP）算成绝对路径再拼入，不依赖任务运行环境。
    $log = Join-Path $env:TEMP 'iuv-cleanup.log'
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

function Test-ArchRegistered {
    # TSF DLL 注册状态检查（install 与 dev-deploy 共用，2026-08-29 下沉）。
    # CLSID 存在 + InprocServer32 指向本 DLL + TIP 键存在；
    # 传 -ProfileDesc 时额外要求显示名一致（区分大小写：-cne/-ceq，-ne 会把 iuv/IUV 判等）。
    param(
        [string]$ClsidKey,
        [string]$TipKey,
        [string]$DllPath,
        [string]$ProfileDesc
    )
    $p = $null
    if (Test-Path "$ClsidKey\InprocServer32") {
        $p = (Get-ItemProperty -Path "$ClsidKey\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
    }
    $ok = ((Test-Path $ClsidKey) -and (Test-Path $TipKey) -and $p -eq $DllPath)
    if ($ok -and $ProfileDesc) {
        $d = (Get-ItemProperty -Path $ClsidKey -ErrorAction SilentlyContinue).'(default)'
        $ok = ($d -cne $null -and $d -ceq $ProfileDesc)
    }
    return $ok
}

# ============================================================
# 以下为部署链公共函数（install / dev-deploy / build 同源；2026-10-02
# 从三份复制收敛于此。输入法的硬约束决定了流程形态：
# - TSF DLL 被全系统进程映射 → 热替换只能改名（Replace-InUseFile），
#   新进程加载新 DLL，运行中进程持旧映射，.old 延迟清理；
# - ctfmon / iuv-server 必须跑在用户中完整性上下文（提权 token 拉起 =
#   "只能输入英文" / 管道拒绝访问，均实测）→ 计划任务受限拉起；
# - 注册走 regsvr32 但被占用时会加载 DLL → 仅在未注册/路径不符时调用。
# ============================================================

# ---- 路径与注册表键唯一来源（两架构 DLL、词库、server、TIP 键）----
function Get-IuvPaths {
    $repoRoot = Split-Path -Parent $PSScriptRoot
    $clsid = '{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
    [pscustomobject]@{
        RepoRoot    = $repoRoot
        ProfileDesc = 'IUV 输入法'   # 与 crates/iuv-tsf/src/registration.rs 一致
        # 构建产物（x64/x86 TSF 同 target；server 独立 target-server）
        DllSrc      = Join-Path $repoRoot 'target\release\iuv_tsf.dll'
        DllSrc32    = Join-Path $repoRoot 'target\i686-pc-windows-msvc\release\iuv_tsf.dll'
        ServerSrc   = Join-Path $repoRoot 'target-server\release\iuv-server.exe'
        # 词库源（缺失时由 Ensure-Imedic / Ensure-Opencc 下载编译）
        ImedicSrc   = Join-Path $repoRoot 'data\iuv.imedic'
        DictsDir    = Join-Path $repoRoot 'data\rime-frost\cn_dicts'
        OpenccSrc   = Join-Path $repoRoot 'data\iuv.opencc'
        OpenccDir   = Join-Path $repoRoot 'data\opencc'
        # 安装目标
        DestDir     = Join-Path $env:ProgramFiles 'iuv'
        DestDll     = Join-Path $env:ProgramFiles 'iuv\iuv_tsf.dll'
        DestDll32   = Join-Path $env:ProgramFiles 'iuv\iuv_tsf_x86.dll'
        ServerDst   = Join-Path $env:ProgramFiles 'iuv\iuv-server.exe'
        DictDir     = Join-Path $env:LOCALAPPDATA 'iuv'
        DictDest    = Join-Path $env:LOCALAPPDATA 'iuv\iuv.imedic'
        OpenccDest  = Join-Path $env:LOCALAPPDATA 'iuv\iuv.opencc'
        ConfigPath  = Join-Path $env:LOCALAPPDATA 'iuv\config.json'
        # 注册表（x64 native + x86 WoW64 视图；32 位 regsvr32 自动落 WoW64）
        ClsidKey    = "Registry::HKEY_CLASSES_ROOT\CLSID\$clsid"
        TipKey      = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\$clsid"
        ClsidKey32  = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\WOW6432Node\CLSID\$clsid"
        TipKey32    = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\CTF\TIP\$clsid"
        Regsvr32    = Join-Path $env:windir 'SysWOW64\regsvr32.exe'
    }
}

# ---- 三车道并行构建（x64 TSF ∥ x86 TSF ∥ server；各自独立 target 互不持锁）----
# **必须在普通（非提权）窗口调用**：提权进程可能换账户/丢 PATH，cargo 会瞬间
# 失败且提权窗口闪退看不到错误（m10 时代实测教训）。调用方先本窗口构建，
# 再提权只做部署。
function Build-All {
    $p = Get-IuvPaths
    Write-Host "三车道并行构建（x64 TSF / x86 TSF / iuv-server）..."
    Push-Location $p.RepoRoot
    try {
        $buildSpecs = @(
            @{ Name = 'x64-tsf'; Env = @{}; CargoArgs = @('build', '-p', 'iuv-tsf', '--release') },
            @{ Name = 'x86-tsf'; Env = @{};
               CargoArgs = @('build', '-p', 'iuv-tsf', '--release', '--target', 'i686-pc-windows-msvc') },
            @{ Name = 'server';  Env = @{ CARGO_TARGET_DIR = (Join-Path $p.RepoRoot 'target-server') };
               CargoArgs = @('build', '-p', 'iuv-server', '--release') }
        )
        $jobs = foreach ($spec in $buildSpecs) {
            Start-Job -Name "iuv-build-$($spec.Name)" -ScriptBlock {
                param($dir, $envMap, $cargoArgs)
                Set-Location $dir
                foreach ($k in $envMap.Keys) { Set-Item -Path "env:$k" -Value $envMap[$k] }
                $out = & cargo @cargoArgs 2>&1
                [pscustomobject]@{ Code = $LASTEXITCODE; Output = ($out | Out-String) }
            } -ArgumentList $p.RepoRoot, $spec.Env, $spec.CargoArgs
        }
        Wait-Job -Job $jobs | Out-Null
        $failed = $false
        foreach ($j in $jobs) {
            $name = $j.Name -replace '^iuv-build-', ''
            $r = Receive-Job -Job $j
            if ($r.Code -ne 0) {
                $failed = $true
                Write-Host ""
                Write-Host "===== cargo build 失败：$name ====="
                $r.Output.TrimEnd()
            } else {
                Trace-Script "Build-All: 车道完成 $name"
            }
        }
        if ($failed) { throw "cargo build 失败（详见上方各车道输出）" }
    } finally {
        Get-Job -Name 'iuv-build-*' -ErrorAction SilentlyContinue | Remove-Job -Force -ErrorAction SilentlyContinue
        Pop-Location
    }
    $missing = @($p.DllSrc, $p.DllSrc32, $p.ServerSrc | Where-Object { -not (Test-Path $_) })
    if ($missing.Count -gt 0) { throw "构建产物缺失：$($missing -join '; ')" }
    Trace-Script "Build-All: 完成（三产物齐全）"
}

# ---- 词库链：iuv.imedic 缺失时下载 + dictc 编译（幂等，产物在即跳过）----
function Ensure-Imedic {
    $p = Get-IuvPaths
    if (Test-Path $p.ImedicSrc) { return }
    Trace-Script "Ensure-Imedic: 词库缺失，进入下载/编译流程"
    if (-not (Test-Path $p.DictsDir) -or ((Get-ChildItem $p.DictsDir -Filter *.dict.yaml -ErrorAction SilentlyContinue).Count -eq 0)) {
        Write-Host "词库源缺失，正在下载（scripts\download-dict.ps1）..."
        Push-Location $p.RepoRoot
        try { & (Join-Path $PSScriptRoot 'download-dict.ps1') } finally { Pop-Location }
    }
    Write-Host "正在编译词库（dictc）..."
    Push-Location $p.RepoRoot
    try {
        $yamlFiles = (Get-ChildItem $p.DictsDir -Filter *.dict.yaml | ForEach-Object { $_.FullName })
        if ($yamlFiles.Count -eq 0) { throw "词库目录为空：$($p.DictsDir)" }
        cargo run -p iuv-data --bin dictc -- $p.ImedicSrc $yamlFiles
        if ($LASTEXITCODE -ne 0) { throw "dictc 编译失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    if (-not (Test-Path $p.ImedicSrc)) { throw "编译完成但未找到 $($p.ImedicSrc)" }
    Trace-Script "Ensure-Imedic: 词库 OK $($p.ImedicSrc)"
}

# ---- 简繁转换表链：iuv.opencc 缺失时下载 + dictc opencc 编译（31-script-traditional.md）----
function Ensure-Opencc {
    $p = Get-IuvPaths
    if (Test-Path $p.OpenccSrc) { return }
    Trace-Script "Ensure-Opencc: iuv.opencc 缺失，进入下载/编译流程"
    if (-not (Test-Path $p.OpenccDir) -or ((Get-ChildItem $p.OpenccDir -Filter *.txt -ErrorAction SilentlyContinue).Count -eq 0)) {
        Write-Host "OpenCC 转换表源缺失，正在下载（scripts\download-opencc.ps1）..."
        Push-Location $p.RepoRoot
        try { & (Join-Path $PSScriptRoot 'download-opencc.ps1') } finally { Pop-Location }
    }
    $phrases = Join-Path $p.OpenccDir 'STPhrases.txt'
    $chars   = Join-Path $p.OpenccDir 'STCharacters.txt'
    if (-not (Test-Path $phrases) -or -not (Test-Path $chars)) {
        throw "OpenCC 转换表源缺失：$($p.OpenccDir)"
    }
    Write-Host "正在编译简繁转换表（dictc opencc）..."
    Push-Location $p.RepoRoot
    try {
        cargo run -p iuv-data --bin dictc -- opencc $p.OpenccSrc $phrases $chars
        if ($LASTEXITCODE -ne 0) { throw "dictc opencc 编译失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    if (-not (Test-Path $p.OpenccSrc)) { throw "编译完成但未找到 $($p.OpenccSrc)" }
    Trace-Script "Ensure-Opencc: iuv.opencc OK $($p.OpenccSrc)"
}

# ---- 生成默认配置（缺失时；带 // 注释，引擎解析兼容 JSONC）----
# 与引擎内默认值同步说明：passthrough/candidate_owner 名单改动须同步
# iuv-server daemon/config.rs 的 DEFAULT_* 常量。
function New-DefaultConfig {
    $p = Get-IuvPaths
    if (Test-Path $p.ConfigPath) { return }
    $template = @'
{
  // 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页）
  "page_size": 5,
  // 候选窗布局：vertical = 竖排（一列）/ horizontal = 横排（单行）
  "candidate_orientation": "vertical",
  // 快捷键映射（41-keymap-settings.md）：双备选键位（主/备两槽，任一可空）。
  // 会话内（翻页/候选移动/调权/隐藏）：仅无修饰/Shift 组合；Alt 不进输入法会话、Ctrl 让给应用。
  // 全局热键（中英/全角/简繁/标点/设置/工具栏）：server RegisterHotKey，Alt/Ctrl 随便绑，须含修饰键。
  "keymap": {
    // 翻上一页：主=PageUp 备=逗号
    "page_prev": { "primary": "PageUp", "secondary": "," },
    // 翻下一页：主=PageDown 备=句号
    "page_next": { "primary": "PageDown", "secondary": "." },
    // 候选前移（页内左移）：主=←
    "candidate_prev": { "primary": "Left", "secondary": null },
    // 候选后移（页内右移）：主=→
    "candidate_next": { "primary": "Right", "secondary": null },
    // 调权（与左侧候选交换权重）：Shift+←
    "swap_left": { "primary": "Shift+Left", "secondary": null },
    // 调权（与右侧候选交换权重）：Shift+→
    "swap_right": { "primary": "Shift+Right", "secondary": null },
    // 隐藏候选：Shift+Delete
    "hide_candidate": { "primary": "Shift+Delete", "secondary": null },
    // 全局热键默认全空（不预占全局键，用户自行在设置页绑定）
    "toggle_mode": { "primary": null, "secondary": null },
    "toggle_width": { "primary": null, "secondary": null },
    "toggle_script": { "primary": null, "secondary": null },
    "toggle_punct": { "primary": null, "secondary": null },
    "open_settings": { "primary": null, "secondary": null },
    "toggle_toolbar": { "primary": null, "secondary": null }
  },
  // 前缀联想（高级）：false = 候选仅精确匹配（默认）/ true = 追加前缀长词
  "candidate_prefix": false,
  // 按键直通进程（高级）：近五年 3A 单机大作——全程无中文输入需求，整进程隐身换零按键干扰。
  "passthrough_apps": [
    "Cyberpunk2077.exe",
    "b1-Win64-Shipping.exe",
    "b1.exe",
    "eldenring.exe",
    "bg3.exe",
    "RDR2.exe",
    "MonsterHunterWilds.exe",
    "Starfield.exe"
  ],
  // 候选渲染自持进程（高级）：这些 app 自己绘制候选栏（如 WoW 游戏内候选框）→ iuv 不绘制
  // 自绘候选窗，数据经候选 UI 元素供其拉取（要打中文的游戏用本名单而非按键直通）。
  "candidate_owner_apps": [
    "wow.exe",
    "WowClassic.exe",
    "Diablo IV.exe",
    "Diablo III64.exe",
    "League of Legends.exe",
    "TslGame.exe",
    "Gw2-64.exe",
    "JX3ClientX64.exe",
    "JX3Client.exe",
    "crossfire.exe"
  ],
  // 新 TSF 实例初始状态：mode = 中文/英文、width = 半角/全角、
  // script = 简体/繁体、punct = 中文标点/英文标点（均仅存默认值）
  "initial_state": {
    "mode": "chinese",
    "width": "half",
    "script": "simplified",
    "punct": "chinese"
  }
}
'@
    # 其余字段（max_candidates/max_word_syllables 等）缺省自动补默认，无需写出
    New-Item -ItemType Directory -Force -Path $p.DictDir | Out-Null
    [IO.File]::WriteAllText($p.ConfigPath, $template, [Text.UTF8Encoding]::new($false))
    Trace-Script "New-DefaultConfig: 生成默认配置 $($p.ConfigPath)"
    Write-Host "已生成默认配置（可编辑注释后改设置）：$($p.ConfigPath)"
}

# ---- 双架构 TSF 注册（各自未注册/路径不符/显示名不符才重注册）----
# 系统按进程架构自动加载对应 DLL，两架构注册独立校验（x64 native + x86 WoW64 视图）。
function Register-IuvTip {
    $p = Get-IuvPaths
    $archRegs = @(
        @{ Dll = $p.DestDll;  Regsvr = "$env:windir\System32\regsvr32.exe"; Clsid = $p.ClsidKey;   Tip = $p.TipKey },
        @{ Dll = $p.DestDll32; Regsvr = $p.Regsvr32;                        Clsid = $p.ClsidKey32; Tip = $p.TipKey32 }
    )
    foreach ($ar in $archRegs) {
        if (Test-ArchRegistered -ClsidKey $ar.Clsid -TipKey $ar.Tip -DllPath $ar.Dll -ProfileDesc $p.ProfileDesc) {
            Trace-Script "Register-IuvTip: 已注册且路径/显示名匹配，跳过 regsvr32（$($ar.Dll)）"
            continue
        }
        Trace-Script "Register-IuvTip: 开始 regsvr32 $($ar.Dll)"
        Write-Host "正在注册 COM/TSF 服务（$($ar.Dll)）..."
        & $ar.Regsvr /s $ar.Dll
        Start-Sleep -Seconds 1
        # 不依赖 regsvr32 退出码（被占用时会加载 DLL 且 $LASTEXITCODE 可能为 $null）；
        # 以注册表写入结果为准。
        if (-not (Test-ArchRegistered -ClsidKey $ar.Clsid -TipKey $ar.Tip -DllPath $ar.Dll -ProfileDesc $p.ProfileDesc)) {
            Trace-Script "Register-IuvTip: 注册失败（CLSID=$(Test-Path $ar.Clsid) TIP=$(Test-Path $ar.Tip)）"
            Write-Host "错误：注册失败（$($ar.Dll)）。日志见 %TEMP%\iuv-script.log"
            return $false
        }
        Trace-Script "Register-IuvTip: regsvr32 完成（$($ar.Dll)）"
    }
    return $true
}

# ---- iuv-server 部署（停旧优雅 → 拷贝 → 常驻登录自启 → 启动 → 验证）----
# 自启任务必须 RunLevel Limited：**用户中完整性上下文**运行——提权脚本直接
# Start-Process 会创建高完整性管道，TSF 客户端（中完整性进程）连接即拒绝
# 访问（实测 2026-09-27）。ExecutionTimeLimit 清零：计划任务默认 3 天限时会
# 杀长驻服务进程。任务常驻注册不注销（2026-09-30：一次性任务导致重启后
# server 缺席——开机自启是服务在位的正道）。
function Deploy-IuvServer {
    $p = Get-IuvPaths
    if (-not (Test-Path $p.ServerSrc)) {
        Write-Host "错误：未找到 $($p.ServerSrc)（先跑 scripts\build.ps1）"
        return $false
    }
    if (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue) {
        Stop-IuvServerGraceful | Out-Null
    }
    New-Item -ItemType Directory -Force -Path $p.DestDir | Out-Null
    Copy-Item $p.ServerSrc $p.ServerDst -Force -ErrorAction Stop
    Trace-Script "Deploy-IuvServer: 复制成功 $($p.ServerDst)"
    $tn = 'Iuv-ServerStart'
    try {
        $u = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        $action = New-ScheduledTaskAction -Execute $p.ServerDst -WorkingDirectory $p.DestDir
        $trigger = New-ScheduledTaskTrigger -AtLogOn -User $u
        $principal = New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited
        $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero)
        Register-ScheduledTask -TaskName $tn -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force -ErrorAction Stop | Out-Null
        Start-ScheduledTask -TaskName $tn -ErrorAction Stop
        Trace-Script "Deploy-IuvServer: 自启任务已常驻注册并启动（AtLogOn $u）"
    } catch {
        Trace-Script "Deploy-IuvServer: 计划任务注册/启动失败：$_"
        Write-Host "警告：计划任务注册/启动失败（$_），回退直启（可能高完整性不可连）"
        Start-Process -FilePath $p.ServerDst -WorkingDirectory $p.DestDir -WindowStyle Hidden
    }
    Start-Sleep -Milliseconds 800
    if (Get-Process -Name 'iuv-server' -ErrorAction SilentlyContinue) {
        Trace-Script "Deploy-IuvServer: iuv-server 已运行（用户中完整性上下文）"
        Write-Host "iuv-server 已部署并启动（日志：%TEMP%\iuv-server.log）"
        return $true
    }
    Trace-Script "Deploy-IuvServer: iuv-server 启动后退出"
    Write-Host "警告：iuv-server 启动后退出，请查看 %TEMP%\iuv-server.log"
    return $false
}
