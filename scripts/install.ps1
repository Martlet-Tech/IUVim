# 安装 iuv 输入法：构建检查 → 词库链 → 安装 DLL/词库 → 注册 → 重启 ctfmon。
# 需管理员权限（自动弹 UAC 提权）。用法：scripts\install.ps1
#
# 设计要点：
# - 不杀进程、不要求关闭应用：DLL 被占用时登记延迟替换（注销/重启后生效，SYSTEM 权限）。
# - 已注册时跳过 regsvr32。
# - ctfmon 经受限计划任务重启，避免提升 token 破坏 TSF（"只能输入英文"问题）。
#requires -Version 5.1

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')
Exit-IfNotAdmin -ScriptPath $PSCommandPath

Trace-Script "install: 提升实例启动"
Write-Host "正在安装 IUV 输入法（管理员窗口）..."

$repoRoot   = Split-Path -Parent $PSScriptRoot
$profileDesc = 'IUV 输入法'   # 显示名（与 registration.rs PROFILE_DESCRIPTION 一致）
$dllSrc     = Join-Path $repoRoot "target\release\iuv_tsf.dll"
$dllSrc32   = Join-Path $repoRoot "target\i686-pc-windows-msvc\release\iuv_tsf.dll"
$imedicSrc  = Join-Path $repoRoot "data\iuv.imedic"
$dictsDir   = Join-Path $repoRoot "data\rime-frost\cn_dicts"
$openccSrc  = Join-Path $repoRoot "data\iuv.opencc"
$openccDir  = Join-Path $repoRoot "data\opencc"
$destDir    = Join-Path $env:ProgramFiles "iuv"              # DLL：程序文件位置
$destDll    = Join-Path $destDir "iuv_tsf.dll"               # x64
$destDll32  = Join-Path $destDir "iuv_tsf_x86.dll"           # x86（M7 双架构）
$dictDir    = Join-Path $env:LOCALAPPDATA "iuv"              # 词库：用户级数据
$dictDest   = Join-Path $dictDir "iuv.imedic"
$openccDest = Join-Path $dictDir "iuv.opencc"
$clsidKey   = 'Registry::HKEY_CLASSES_ROOT\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey     = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
# x86 注册走 WoW64 视图（32 位 regsvr32 自动落此，64 位进程按架构解析 DLL）：
# HKLM\SOFTWARE\Classes\WOW6432Node\CLSID = HKCR\WOW6432Node\CLSID（同一物理键）。
$clsidKey32 = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\WOW6432Node\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey32   = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$regsvr32Path = Join-Path $env:windir "SysWOW64\regsvr32.exe"   # 32 位注册器（注册 x86 DLL）

# ---- 0.5 DLL 安装辅助：未锁直接复制；被占用 → 临时名 + 延迟替换（注销/重启后生效）----
function Install-DllFile {
    param(
        [Parameter(Mandatory)][string]$Src,
        [Parameter(Mandatory)][string]$Dest
    )
    try {
        Copy-Item $Src $Dest -Force -ErrorAction Stop
        Trace-Script "install: DLL 复制成功 $Dest"
        Write-Host "已安装 DLL：$Dest"
        return $false
    } catch {
        $base = (Split-Path $Dest -Leaf) -replace '\.dll$', ''
        $tmp = Join-Path (Split-Path $Dest) "$base.new.dll"
        Copy-Item $Src $tmp -Force
        Trace-Script "install: DLL 被占用，延迟替换 $tmp -> $Dest"
        Add-PendingOp -Source $tmp -Dest $Dest
        if (Register-DelayedOps -Copies @($tmp, $Dest) -Deletes @($tmp)) {
            Write-Host "DLL 正被占用（无需关闭应用），已安排延迟替换，注销或重启后生效：$Dest"
            return $true
        }
        throw "DLL 被占用且延迟替换任务注册失败，请重启后重新运行本脚本"
    }
}

# ---- 1. 构建检查（x64 + x86 双产物）----
$missing = @()
if (-not (Test-Path $dllSrc)) { $missing += "x64：$dllSrc（cargo build -p iuv-tsf --release）" }
if (-not (Test-Path $dllSrc32)) { $missing += "x86：$dllSrc32（cargo build -p iuv-tsf --release --target i686-pc-windows-msvc）" }
if ($missing.Count -gt 0) {
    foreach ($m in $missing) { Trace-Script "install: 错误，构建产物缺失 $m" }
    Write-Host "错误：以下构建产物缺失："
    $missing | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Trace-Script "install: 构建产物 OK（x64+x86）"
Write-Host "构建产物 OK（x64 + x86）：$dllSrc"

# ---- 2. 词库链：imedic 缺失时自动下载 + 编译 ----
if (-not (Test-Path $imedicSrc)) {
    Trace-Script "install: 词库缺失，进入下载/编译流程"
    if (-not (Test-Path $dictsDir) -or ((Get-ChildItem $dictsDir -Filter *.dict.yaml).Count -eq 0)) {
        Write-Host "词库源缺失，正在下载（scripts\download-dict.ps1）..."
        Push-Location $repoRoot
        try { & (Join-Path $PSScriptRoot "download-dict.ps1") }
        finally { Pop-Location }
    }
    Write-Host "正在编译词库（dictc）..."
    Push-Location $repoRoot
    try {
        $yamlFiles = (Get-ChildItem $dictsDir -Filter *.dict.yaml | ForEach-Object { $_.FullName })
        if ($yamlFiles.Count -eq 0) { throw "词库目录为空：$dictsDir" }
        cargo run -p iuv-data --bin dictc -- $imedicSrc $yamlFiles
        if ($LASTEXITCODE -ne 0) { throw "dictc 编译失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    if (-not (Test-Path $imedicSrc)) { throw "编译完成但未找到 $imedicSrc" }
}
Trace-Script "install: 词库 OK"
Write-Host "词库 OK：$imedicSrc"

# ---- 2.5 简繁转换表链：iuv.opencc 缺失时自动下载 + 编译（31-script-traditional.md）----
if (-not (Test-Path $openccSrc)) {
    Trace-Script "install: iuv.opencc 缺失，进入下载/编译流程"
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
Trace-Script "install: iuv.opencc OK"
Write-Host "简繁转换表 OK：$openccSrc"

# ---- 3. 安装 DLL（x64 + x86 双架构，各自被占用时登记延迟替换）----
# 先清除指向安装目录的陈旧 pending op（旧版无去重卸载可能留下"重启删目录"条目，
# 不清理的话下次重启会删掉刚装的目录）。
$cleared = Clear-PendingOp -Source $destDir
Trace-Script ("install: 陈旧 pending op 清理=" + $cleared)
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$queuedReplace = $false
if (Install-DllFile -Src $dllSrc -Dest $destDll) { $queuedReplace = $true }
if (Install-DllFile -Src $dllSrc32 -Dest $destDll32) { $queuedReplace = $true }

# ---- 4. 安装词库（用户级数据；目标可能被引擎进程 mmap——mmap 声明 FILE_SHARE_DELETE
#        可改名但截断写被拒 ERROR_USER_MAPPED_FILE → 走 Replace-InUseFile 改名替换）----
New-Item -ItemType Directory -Force -Path $dictDir | Out-Null
$r = Replace-InUseFile -Src $imedicSrc -Dest $dictDest
if (-not $r.Ok) {
    Trace-Script "install: 词库替换失败（$dictDest）"
    Write-Host "错误：词库替换失败：$dictDest（多为引擎进程/搜索索引器占用，注销重启后重跑）"
    exit 1
}
if ($r.Renamed) {
    Write-Host "词库已替换（旧版被占用，已改名 $($r.OldPath)）：注销/重启后新进程加载新词库。"
}
Trace-Script "install: 词库替换 $dictDest"
Write-Host "已安装词库：$dictDest"

# ---- 4.25 安装简繁转换表（iuv.opencc，同样走 Replace-InUseFile：引擎 mmap 锁处理）----
$r2 = Replace-InUseFile -Src $openccSrc -Dest $openccDest
if (-not $r2.Ok) {
    Trace-Script "install: iuv.opencc 替换失败（$openccDest）"
    Write-Host "警告：简繁转换表替换失败（$openccDest），繁体模式将降级简体输出。注销重启后重跑。"
} else {
    if ($r2.Renamed) {
        Write-Host "简繁转换表已替换（旧版被占用，已改名）：注销/重启后新进程加载新表。"
    }
    Trace-Script "install: iuv.opencc 替换 $openccDest"
    Write-Host "已安装简繁转换表：$openccDest"
}

# ---- 4.5 生成默认配置（缺失时；带 // 注释，引擎解析兼容 JSONC）----
$configPath = Join-Path $dictDir "config.json"
if (-not (Test-Path $configPath)) {
    $template = @'
{
  // 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页）
  "page_size": 5,
  // 候选窗布局：vertical = 竖排（一列）/ horizontal = 横排（单行）
  "candidate_orientation": "vertical",
  // 快捷键映射（41-keymap-settings.md）：双备选键位（主/备两槽，任一可空）。
  // 会话内（翻页/候选移动/调权/隐藏）：仅无修饰/Shift 组合；Alt 不进输入法会话、Ctrl 让给应用。
  // 全局热键（中英/全角/简繁/标点/设置/工具栏）：daemon RegisterHotKey，Alt/Ctrl 随便绑，须含修饰键。
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
  // 与 daemon config.rs DEFAULT_PASSTHROUGH_APPS 保持同步。
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
  // 与 daemon config.rs DEFAULT_CANDIDATE_OWNER_APPS 保持同步；设置页「恢复默认名单」同源。
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
    # 其余字段（max_candidates/max_word_syllables 等）缺省自动补默认，无需写出
    [IO.File]::WriteAllText($configPath, $template, [Text.UTF8Encoding]::new($false))
    Trace-Script "install: 生成默认配置 $configPath"
    Write-Host "已生成默认配置（可编辑注释后改设置）：$configPath"
}

# ---- 5. 注册（x64 native 视图 + x86 WOW6432Node 视图；各自未注册/路径不符才重注册）----
# 两个架构各自判断：系统按进程架构自动加载对应 DLL，注册要独立校验。
$archRegs = @(
    @{ Reg32 = $false; Dll = $destDll;  Regsvr = "$env:windir\System32\regsvr32.exe"; Clsid = $clsidKey;   Tip = $tipKey },
    @{ Reg32 = $true;  Dll = $destDll32; Regsvr = $regsvr32Path;                       Clsid = $clsidKey32; Tip = $tipKey32 }
)
foreach ($ar in $archRegs) {
    if (Test-ArchRegistered -ClsidKey $ar.Clsid -TipKey $ar.Tip -DllPath $ar.Dll -ProfileDesc $profileDesc) {
        Trace-Script "install: 已注册，跳过 regsvr32（$($ar.Dll)）"
        Write-Host "已注册（跳过 regsvr32）：$($ar.Dll)"
        continue
    }
    Trace-Script "install: 开始 regsvr32 $($ar.Dll)（32位=$($ar.Reg32)）"
    Write-Host "正在注册 COM/TSF 服务（$($ar.Dll)）..."
    & $ar.Regsvr /s $ar.Dll
    Start-Sleep -Seconds 1
    Trace-Script ("install: regsvr32 返回，CLSID=" + (Test-Path $ar.Clsid) + " TIP=" + (Test-Path $ar.Tip))
    # 不依赖 regsvr32 退出码（$LASTEXITCODE 可能为 $null）；以注册表是否写入为准。
    if (-not (Test-ArchRegistered -ClsidKey $ar.Clsid -TipKey $ar.Tip -DllPath $ar.Dll)) {
        Write-Host "错误：注册失败（CLSID=$(Test-Path $ar.Clsid) TIP=$(Test-Path $ar.Tip)）。日志见 %TEMP%\iuv-tsf.log"
        exit 1
    }
}

# ---- 6. 重启 ctfmon（受限用户上下文）----
if (-not (Restart-Ctfmon)) {
    Trace-Script "install: ctfmon 重启失败"
    Write-Host "警告：自动重启 ctfmon 失败。请手动重启（任务管理器结束 ctfmon.exe 后，新建任务运行 ctfmon.exe），或注销/重启后生效。"
} else {
    Trace-Script "install: ctfmon 已重启"
}

if ($queuedReplace) { Write-Host "注意：新版本 DLL 将在注销或重启后生效。" }
Trace-Script "install: 安装完成"
Write-Host "安装完成。下一步：Windows 设置 → 时间和语言 → 语言 → 中文 → 键盘 → 切换到 'IUV 输入法'"
