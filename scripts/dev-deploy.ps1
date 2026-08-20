# 开发热部署（智能体/开发者用）：构建 → 热替换 DLL + 词库 → 重启 ctfmon。
# 智能体使用场景：改完 Rust 代码后跑本脚本即可让新构建生效（新进程加载新 DLL），无需注销/重启。
# 与 install.ps1 的区别：DLL 被占用时不杀进程、不登记"重启后替换"，而是改名 .old 原位
# 替换即时生效（新进程加载新 DLL），老进程继续持旧映射直到自然退出。全程不注销不重启。
# 需管理员权限（自动弹 UAC 提权）。
#
# 用法：scripts\dev-deploy.ps1            # 先 cargo build -p iuv-tsf --release 再部署
#       scripts\dev-deploy.ps1 -SkipBuild # 跳过构建，只部署现有产物
#
# 局限：运行中的其他应用（浏览器/编辑器等）仍持旧 DLL，需重启这些应用后才生效。
#requires -Version 5.1

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
$pass = @()
if ($SkipBuild) { $pass = @('-SkipBuild') }
Exit-IfNotAdmin -ScriptPath $PSCommandPath -PassArgs $pass

Trace-Script "dev-deploy: 提升实例启动"
Write-Host "正在热部署 IUV 输入法（管理员窗口）..."

$repoRoot  = Split-Path -Parent $PSScriptRoot
$dllSrc    = Join-Path $repoRoot "target\release\iuv_tsf.dll"
$dllSrc32  = Join-Path $repoRoot "target\i686-pc-windows-msvc\release\iuv_tsf.dll"
$imedicSrc = Join-Path $repoRoot "data\iuv.imedic"
$openccSrc = Join-Path $repoRoot "data\iuv.opencc"
$openccDir = Join-Path $repoRoot "data\opencc"
$destDir   = Join-Path $env:ProgramFiles "iuv"
$destDll   = Join-Path $destDir "iuv_tsf.dll"
$destDll32 = Join-Path $destDir "iuv_tsf_x86.dll"
$dictDir   = Join-Path $env:LOCALAPPDATA "iuv"
$dictDest  = Join-Path $dictDir "iuv.imedic"
$openccDest = Join-Path $dictDir "iuv.opencc"
$clsidKey  = 'Registry::HKEY_CLASSES_ROOT\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey    = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
# x86 注册走 WoW64 视图（32 位 regsvr32 自动落此，64 位进程按架构解析 DLL）。
$clsidKey32 = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\WOW6432Node\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey32   = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$regsvr32Path = Join-Path $env:windir "SysWOW64\regsvr32.exe"

# ---- 1. 构建（默认执行，增量很快；-SkipBuild 跳过）----
if (-not $SkipBuild) {
    Trace-Script "dev-deploy: 开始构建 cargo build -p iuv-tsf --release"
    Write-Host "正在构建（cargo build -p iuv-tsf --release）..."
    Push-Location $repoRoot
    try {
        cargo build -p iuv-tsf --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（exit=$LASTEXITCODE）" }
        cargo build -p iuv-tsf --release --target i686-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw "cargo build x86 失败（exit=$LASTEXITCODE）" }
        # 守护进程（dev 构建标记：设置页带「开发者」标签/清除日志；发布安装不带）。
        cargo build -p iuv-daemon --release --features dev
        if ($LASTEXITCODE -ne 0) { throw "cargo build iuv-daemon 失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    Trace-Script "dev-deploy: 构建完成"
} else {
    Trace-Script "dev-deploy: -SkipBuild，跳过构建"
}

# ---- 2. 产物检查 ----
$missing = @()
if (-not (Test-Path $dllSrc)) { $missing += "x64：$dllSrc（cargo build -p iuv-tsf --release）" }
if (-not (Test-Path $dllSrc32)) { $missing += "x86：$dllSrc32（cargo build -p iuv-tsf --release --target i686-pc-windows-msvc）" }
if ($missing.Count -gt 0) {
    foreach ($m in $missing) { Trace-Script "dev-deploy: 错误，构建产物缺失 $m" }
    Write-Host "错误：以下构建产物缺失："
    $missing | ForEach-Object { Write-Host "  $_" }
    Write-Host "请先构建（或去掉 -SkipBuild）"
    exit 1
}
if (-not (Test-Path $imedicSrc)) {
    Trace-Script "dev-deploy: 错误，词库缺失 $imedicSrc"
    Write-Host "错误：未找到 $imedicSrc"
    Write-Host "请先执行：scripts\install.ps1 或 scripts\download-dict.ps1 生成词库"
    exit 1
}

New-Item -ItemType Directory -Force -Path $destDir | Out-Null
New-Item -ItemType Directory -Force -Path $dictDir | Out-Null

# ---- 2.5 生成默认配置（缺失时；带 // 注释，引擎解析兼容 JSONC）----
$configPath = Join-Path $dictDir "config.json"
if (-not (Test-Path $configPath)) {
    $template = @'
{
  // 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页）
  "page_size": 5,
  // 候选窗布局：vertical = 竖排（一列）/ horizontal = 横排（单行）
  "candidate_orientation": "vertical",
  // 快捷键四组语义键（键位与布局方向解耦，改布局后按需自行调整）
  "keymap": {
    // 前页（上翻页）：默认 ↑ / PageUp / 逗号
    "page_prev": ["PageUp", ",", "Up"],
    // 后页（下翻页）：默认 ↓ / PageDown / 句号
    "page_next": ["PageDown", ".", "Down"],
    // 前一个候选项（页内左移/上移）：默认 ←
    "candidate_prev": ["Left"],
    // 后一个候选项（页内右移/下移）：默认 →
    "candidate_next": ["Right"]
  },
  // 前缀联想（高级）：false = 候选仅精确匹配（默认）/ true = 追加前缀长词
  "candidate_prefix": false,
  // 候选渲染自持进程（高级）：这些 app 自己绘制候选栏（如 WoW 游戏内候选框）→ iuv 不绘制
  // 自绘候选窗。默认预置 wow.exe（大小写不敏感精确匹配）；其他 app 用户自行追加。
  "candidate_owner_apps": ["wow.exe"],
  // 新 TSF 实例初始状态（2026-08-19 起，替换旧顶层 english_punctuation）：
  // mode = 中文/英文、width = 半角/全角（仅存默认值）、script = 简体/繁体（仅存默认值）、
  // punct = 中文标点/英文标点（中文状态按标点键直通英文形）
  "initial_state": {
    "mode": "chinese",
    "width": "half",
    "script": "simplified",
    "punct": "chinese"
  }
}
'@
    [IO.File]::WriteAllText($configPath, $template, [Text.UTF8Encoding]::new($false))
    Trace-Script "dev-deploy: 生成默认配置 $configPath"
    Write-Host "已生成默认配置（可编辑注释后改设置）：$configPath"
}

# ---- 2.5 简繁转换表链（31-script-traditional.md）：iuv.opencc 缺失时自动下载 + 编译 ----
if (-not (Test-Path $openccSrc)) {
    Trace-Script "dev-deploy: iuv.opencc 缺失，进入下载/编译流程"
    if (-not (Test-Path $openccDir) -or ((Get-ChildItem $openccDir -Filter *.txt).Count -eq 0)) {
        Write-Host "OpenCC 转换表源缺失，正在下载（scripts\download-opencc.ps1）..."
        Push-Location $repoRoot
        try { & (Join-Path $PSScriptRoot "download-opencc.ps1") }
        finally { Pop-Location }
    }
    $phrases = Join-Path $openccDir "STPhrases.txt"
    $chars   = Join-Path $openccDir "STCharacters.txt"
    if (-not (Test-Path $phrases) -or -not (Test-Path $chars)) {
        throw "OpenCC 转换表源缺失：$openccDir"
    }
    Write-Host "正在编译简繁转换表（dictc opencc）..."
    Push-Location $repoRoot
    try {
        cargo run -p iuv-data --bin dictc -- opencc $openccSrc $phrases $chars
        if ($LASTEXITCODE -ne 0) { throw "dictc opencc 编译失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    if (-not (Test-Path $openccSrc)) { throw "编译完成但未找到 $openccSrc" }
}
Trace-Script "dev-deploy: iuv.opencc OK"

# ---- 3. 复制（DLL 用热替换：未锁直接复制，锁了改名 .old 原位替换，零杀进程）----
# 词库同样走 Replace-InUseFile：mmap 声明 FILE_SHARE_DELETE 可改名但不可截断写
# （ERROR_USER_MAPPED_FILE）→ 直接覆盖失败时自动改名 .old + 原位拷新，老进程持旧映射、
# 新进程取新词库；失败只警告不阻断 DLL 热替换（词库与 DLL 是两个独立产物）。
$r = Replace-InUseFile -Src $imedicSrc -Dest $dictDest -WarnOnly
if ($r.Ok) {
    if ($r.Renamed) {
        Write-Host "词库已替换（旧版被占用，已改名 $($r.OldPath)）：新进程加载新词库，老进程持旧映射。"
        Write-Host "  若测试仍无新词库效果，请在新开窗口/重启应用后进行。"
    } else {
        Trace-Script "dev-deploy: 词库替换成功（直接覆盖）$dictDest"
    }
} else {
    Trace-Script "dev-deploy: 词库替换失败（$dictDest），本次仅部署 DLL"
    Write-Host "警告：词库替换失败（$dictDest），本次仅部署 DLL。"
    Write-Host "  原因多为引擎进程/搜索索引器占用；注销重启后重跑本脚本即可更新词库。"
}
$r = Replace-InUseFile -Src $openccSrc -Dest $openccDest -WarnOnly
if ($r.Ok) {
    if ($r.Renamed) {
        Write-Host "简繁转换表已替换（旧版被占用，已改名 $($r.OldPath)）：新进程加载新表，老进程持旧映射。"
    } else {
        Trace-Script "dev-deploy: 简繁转换表替换成功（直接覆盖）$openccDest"
    }
} else {
    Trace-Script "dev-deploy: 简繁转换表替换失败（$openccDest），本次仅部署 DLL"
    Write-Host "警告：简繁转换表替换失败（$openccDest），繁体模式将降级简体输出。"
}
$r = Replace-InUseFile -Src $dllSrc -Dest $destDll
if (-not $r.Ok) { exit 1 }
if ($r.Renamed) {
    Write-Host "DLL 被占用，已用改名替换：老进程仍用旧 DLL，新进程将加载新 DLL（.old 将在注销/重启后自动清理）。"
} else {
    Trace-Script "dev-deploy: DLL 复制成功 $destDll"
}
$r32 = Replace-InUseFile -Src $dllSrc32 -Dest $destDll32
if (-not $r32.Ok) { exit 1 }
if ($r32.Renamed) {
    Write-Host "x86 DLL 被占用，已用改名替换：32 位进程需重启后加载新 DLL（.old 将在注销/重启后自动清理）。"
} else {
    Trace-Script "dev-deploy: x86 DLL 复制成功 $destDll32"
}

# ---- 3.5 守护进程部署（M7：iuv-daemon.exe；会话进程首激活自动拉起）----
$daemonSrc  = Join-Path $repoRoot "target\release\iuv-daemon.exe"
$destDaemon = Join-Path $destDir "iuv-daemon.exe"
if (Test-Path $daemonSrc) {
    # 先停运行中的 daemon（复制会锁；下次会话激活自动拉起新版本）。
    $daemonProc = Get-Process -Name "iuv-daemon" -ErrorAction SilentlyContinue
    if ($daemonProc) {
        Trace-Script "dev-deploy: 停止运行中的 iuv-daemon（PID=$($daemonProc.Id -join ',')）"
        Stop-Process -Name "iuv-daemon" -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }
    try {
        Copy-Item $daemonSrc $destDaemon -Force -ErrorAction Stop
        Trace-Script "dev-deploy: 守护进程复制成功 $destDaemon"
        Write-Host "守护进程已部署（下次切换输入法自动拉起）：$destDaemon"
    } catch {
        Trace-Script "dev-deploy: 守护进程复制失败（$destDaemon）：$($_.Exception.Message)"
        Write-Host "警告：守护进程复制失败（$destDaemon），本次仅部署 DLL。"
    }
} else {
    Trace-Script "dev-deploy: 未找到守护进程产物 $daemonSrc（第 1 步已自动构建 cargo build -p iuv-daemon --release --features dev）"
}

# ---- 4. 注册（x64 native + x86 WOW6432Node；各自未注册或 CLSID 指向路径不符时重注册）----
# 曾因只查 key 存在与否而跳过 regsvr32，导致注册表仍指向旧路径的旧 DLL
# （项目改名前的 C:\Program Files\InputIME），热部署永远不生效。现与 install.ps1
# 同款校验：InprocServer32 默认值必须等于本安装的 destDll（每架构独立判定）。
function Test-ArchRegistered {
    param([string]$ClsidKey, [string]$TipKey, [string]$DllPath)
    $p = $null
    if (Test-Path "$ClsidKey\InprocServer32") {
        $p = (Get-ItemProperty -Path "$ClsidKey\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
    }
    return ((Test-Path $ClsidKey) -and (Test-Path $TipKey) -and $p -eq $DllPath)
}

$archRegs = @(
    @{ Dll = $destDll;  Regsvr = "$env:windir\System32\regsvr32.exe"; Clsid = $clsidKey;   Tip = $tipKey },
    @{ Dll = $destDll32; Regsvr = $regsvr32Path;                       Clsid = $clsidKey32; Tip = $tipKey32 }
)
foreach ($ar in $archRegs) {
    $registeredPath = $null
    if (Test-Path "$($ar.Clsid)\InprocServer32") {
        $registeredPath = (Get-ItemProperty -Path "$($ar.Clsid)\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
    }
    if (Test-ArchRegistered -ClsidKey $ar.Clsid -TipKey $ar.Tip -DllPath $ar.Dll) {
        Trace-Script "dev-deploy: 已注册且路径匹配，跳过 regsvr32（$($ar.Dll)）"
        continue
    }
    Trace-Script "dev-deploy: 开始 regsvr32 $($ar.Dll)（注册路径=$registeredPath）"
    Write-Host "正在注册 COM/TSF 服务（$($ar.Dll)）..."
    & $ar.Regsvr /s $ar.Dll
    Start-Sleep -Seconds 1
    $afterPath = (Get-ItemProperty -Path "$($ar.Clsid)\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
    if (-not (Test-ArchRegistered -ClsidKey $ar.Clsid -TipKey $ar.Tip -DllPath $ar.Dll)) {
        Trace-Script "dev-deploy: 注册失败（CLSID=$(Test-Path $ar.Clsid) TIP=$(Test-Path $ar.Tip) path=$afterPath）"
        Write-Host "错误：注册失败（$($ar.Dll)）。日志见 %TEMP%\iuv-script.log"
        exit 1
    }
    Trace-Script "dev-deploy: regsvr32 完成，CLSID=True TIP=True path=$afterPath"
}

# ---- 5. 重启 ctfmon（受限用户上下文，加载新 DLL）----
if (-not (Restart-Ctfmon)) {
    Trace-Script "dev-deploy: ctfmon 重启失败"
    Write-Host "警告：ctfmon 重启失败，注销/重启后自动生效。"
} else {
    Trace-Script "dev-deploy: ctfmon 已重启"
}

Trace-Script "dev-deploy: 部署完成"
Write-Host ""
Write-Host "热部署完成：DLL + 词库已更新为最新构建。"
Write-Host "注意：正在运行的其他应用仍使用旧 DLL，测试请在新窗口/重启应用后进行。"
