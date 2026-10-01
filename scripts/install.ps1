# 安装 iuv 输入法（全新机器 / 重装）：构建产物检查 → 词库链 → 安装 → 注册 →
# 重启 ctfmon → 引导切换输入法。需管理员权限（自动弹 UAC 提权）。
#
# 与 dev-deploy.ps1 的分野：本脚本面向全新机器"做该做的"——词库/简繁表缺失时
# 自动下载编译、server 部署 + 登录自启、注册、ctfmon 重启、最后引导用户在系统
# 设置里切换到本输入法。日常迭代请用 dev-deploy.ps1（热替换，免注销）。
#
# 设计要点（输入法硬约束）：
# - 不杀进程、不要求关闭应用：DLL 被占用走改名替换（老进程持旧映射，新进程
#   加载新 DLL），残留 .old 延迟清理（注销/重启后自动执行）；
# - ctfmon / iuv-server 经受限计划任务在用户会话拉起（提权 token 会破坏 TSF）。
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
Exit-IfNotAdmin -ScriptPath $PSCommandPath

Trace-Script "install: 提升实例启动"
Write-Host "正在安装 IUV 输入法（管理员窗口）..."
$p = Get-IuvPaths

# ---- 1. 构建产物检查（x64/x86 TSF + server 三产物）----
$missing = @()
if (-not (Test-Path $p.DllSrc))    { $missing += "x64 TSF：$($p.DllSrc)" }
if (-not (Test-Path $p.DllSrc32))  { $missing += "x86 TSF：$($p.DllSrc32)" }
if (-not (Test-Path $p.ServerSrc)) { $missing += "server：$($p.ServerSrc)" }
if ($missing.Count -gt 0) {
    Write-Host "错误：以下构建产物缺失："
    $missing | ForEach-Object { Write-Host "  $_" }
    Write-Host "请先运行 scripts\build.ps1（三车道并行构建）"
    exit 1
}
Trace-Script "install: 构建产物 OK（x64+x86+server）"
Write-Host "构建产物 OK：$($p.DllSrc)"

# ---- 2. 词库链：imedic / opencc 缺失时自动下载 + 编译 ----
try {
    Ensure-Imedic
    Ensure-Opencc
} catch {
    Trace-Script "install: 词库链失败：$_"
    Write-Host "错误：$_"
    exit 1
}
Write-Host "词库 OK：$($p.ImedicSrc)"
Write-Host "简繁转换表 OK：$($p.OpenccSrc)"

# ---- 3. 安装（先清指向安装目录的陈旧 pending op——旧版"重启删目录"条目会误删新装）----
$cleared = Clear-PendingOp -Source $p.DestDir
Trace-Script "install: 陈旧 pending op 清理=$cleared"
New-Item -ItemType Directory -Force -Path $p.DestDir | Out-Null
New-Item -ItemType Directory -Force -Path $p.DictDir | Out-Null

foreach ($pair in @(@($p.DllSrc, $p.DestDll), @($p.DllSrc32, $p.DestDll32))) {
    $r = Replace-InUseFile -Src $pair[0] -Dest $pair[1]
    if (-not $r.Ok) { exit 1 }
    if ($r.Renamed) {
        Write-Host "DLL 被占用，已用改名替换：运行中进程仍用旧 DLL，新进程加载新 DLL。"
    }
}
$r = Replace-InUseFile -Src $p.ImedicSrc -Dest $p.DictDest
if (-not $r.Ok) {
    Trace-Script "install: 词库替换失败（$($p.DictDest)）"
    Write-Host "错误：词库替换失败：$($p.DictDest)（多为引擎进程/搜索索引器占用，注销重启后重跑）"
    exit 1
}
if ($r.Renamed) { Write-Host "词库已替换（旧版被占用，已改名 $($r.OldPath)）。" }
Write-Host "已安装词库：$($p.DictDest)"
$r = Replace-InUseFile -Src $p.OpenccSrc -Dest $p.OpenccDest -WarnOnly
if ($r.Ok) {
    if ($r.Renamed) { Write-Host "简繁转换表已替换（旧版被占用，已改名）。" }
    Write-Host "已安装简繁转换表：$($p.OpenccDest)"
} else {
    Write-Host "警告：简繁转换表替换失败（$($p.OpenccDest)），繁体模式将降级简体输出。注销重启后重跑。"
}

# ---- 4. 默认配置（缺失时）----
New-DefaultConfig

# ---- 5. iuv-server 部署（引擎服务进程 + 登录自启）----
if (-not (Deploy-IuvServer)) { exit 1 }

# ---- 6. 注册（双架构；未注册/路径不符/显示名不符才 regsvr32）----
if (-not (Register-IuvTip)) { exit 1 }

# ---- 7. 重启 ctfmon（受限用户上下文）----
if (-not (Restart-Ctfmon)) {
    Trace-Script "install: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请手动重启（任务管理器结束 ctfmon.exe 后，新建任务运行 ctfmon.exe），或注销/重启后生效。"
} else {
    Trace-Script "install: ctfmon 已重启"
}

Trace-Script "install: 安装完成"
Write-Host ""
Write-Host "安装完成。下一步：Windows 设置 → 时间和语言 → 语言 → 中文 → 键盘 → 切换到 'IUV 输入法'"
