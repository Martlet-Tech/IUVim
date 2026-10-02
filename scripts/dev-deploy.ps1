# 开发热部署：运行中替换新文件，**免注销生效**——新开的进程（新记事本/重启的
# 应用）加载新 DLL/词库，运行中进程持旧映射不受干扰。这是它与 install.ps1 的
# 分野：install 面向全新机器做完整安装，本脚本面向日常迭代（默认先构建）。
# 需管理员权限（自动弹 UAC 提权；提权只做部署，构建留在当前窗口）。
#
# 用法：scripts\dev-deploy.ps1            # 三车道并行构建（x64/x86 TSF ∥ server）后部署
#       scripts\dev-deploy.ps1 -SkipBuild # 跳过构建，只部署现有产物
#
# 输入法特殊性（务必如实告知，脚本不假装生效）：TSF DLL 被全系统运行中的
# 应用映射，热替换后**运行中的应用仍持旧 DLL**——测试必须新开窗口/重启应用。
#requires -Version 5.1

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')

# ---- 1. 构建（当前普通窗口；提权进程可能丢 PATH，cargo 会闪退失败——实测教训）----
if (-not $SkipBuild) {
    try {
        Build-All
    } catch {
        Write-Host "错误：$_"
        exit 1
    }
} else {
    Write-Host "-SkipBuild：跳过构建"
    $p0 = Get-IuvPaths
    $missing = @($p0.DllSrc, $p0.DllSrc32, $p0.ServerSrc | Where-Object { -not (Test-Path $_) })
    if ($missing.Count -gt 0) {
        Write-Host "错误：构建产物缺失：$($missing -join '; ')"
        Write-Host "请先跑 scripts\build.ps1（或去掉 -SkipBuild）"
        exit 1
    }
}

# ---- 2. 提权执行部署（部署不需要 cargo）----
Exit-IfNotAdmin -ScriptPath $PSCommandPath -PassArgs @('-SkipBuild')

Trace-Script "dev-deploy: 提升实例启动（SkipBuild=$SkipBuild）"
Write-Host "正在热部署 IUV 输入法（管理员窗口）..."
$p = Get-IuvPaths

# ---- 3. 词库/简繁表链（幂等：产物在即跳过下载编译）----
try {
    Ensure-Imedic
    Ensure-Opencc
} catch {
    Trace-Script "dev-deploy: 词库链失败：$_"
    Write-Host "错误：$_"
    exit 1
}

# ---- 4. 热替换（未锁直接覆盖；被锁改名 .old 原位拷新，延迟清理收尾）----
# 词库用 -WarnOnly：引擎 mmap 占用不阻断 DLL/服务部署（两个独立产物）。
$r = Replace-InUseFile -Src $p.ImedicSrc -Dest $p.DictDest -WarnOnly
if ($r.Ok -and $r.Renamed) {
    Write-Host "词库已替换（旧版被占用，已改名 $($r.OldPath)）：新进程加载新词库。"
} elseif (-not $r.Ok) {
    Write-Host "警告：词库替换失败（$($p.DictDest)），本次仅部署 DLL/server。"
}
$r = Replace-InUseFile -Src $p.OpenccSrc -Dest $p.OpenccDest -WarnOnly
if ($r.Ok -and $r.Renamed) {
    Write-Host "简繁转换表已替换（旧版被占用，已改名 $($r.OldPath)）。"
} elseif (-not $r.Ok) {
    Write-Host "警告：简繁转换表替换失败（$($p.OpenccDest)），繁体模式将降级简体输出。"
}
foreach ($pair in @(@($p.DllSrc, $p.DestDll), @($p.DllSrc32, $p.DestDll32))) {
    $r = Replace-InUseFile -Src $pair[0] -Dest $pair[1]
    if (-not $r.Ok) { exit 1 }
    if ($r.Renamed) {
        Write-Host "DLL 被占用，已用改名替换：运行中进程仍用旧 DLL，新进程加载新 DLL。"
    }
}

# ---- 5. 默认配置（缺失时）----
New-DefaultConfig

# ---- 6. iuv-server 部署（运行中 → 优雅停机广播 Push::Shutdown 再替换）----
if (-not (Deploy-IuvServer)) { exit 1 }

# ---- 7. 注册（未注册/路径不符/显示名不符才 regsvr32）----
if (-not (Register-IuvTip)) { exit 1 }

# ---- 8. 重启 ctfmon（受限用户上下文；新进程即加载新 DLL）----
if (-not (Restart-Ctfmon)) {
    Trace-Script "dev-deploy: ctfmon 重启失败"
    Write-Host "警告：ctfmon 重启失败，注销/重启后自动生效。"
} else {
    Trace-Script "dev-deploy: ctfmon 已重启"
}

Trace-Script "dev-deploy: 部署完成"
Write-Host ""
Write-Host "热部署完成。注意：运行中的应用仍使用旧 DLL，测试请新开窗口（如新记事本）。"
