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
$imedicSrc = Join-Path $repoRoot "data\iuv.imedic"
$destDir   = Join-Path $env:ProgramFiles "iuv"
$destDll   = Join-Path $destDir "iuv_tsf.dll"
$dictDir   = Join-Path $env:LOCALAPPDATA "iuv"
$dictDest  = Join-Path $dictDir "iuv.imedic"
$clsidKey  = 'Registry::HKEY_CLASSES_ROOT\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey    = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'

# ---- 1. 构建（默认执行，增量很快；-SkipBuild 跳过）----
if (-not $SkipBuild) {
    Trace-Script "dev-deploy: 开始构建 cargo build -p iuv-tsf --release"
    Write-Host "正在构建（cargo build -p iuv-tsf --release）..."
    Push-Location $repoRoot
    try {
        cargo build -p iuv-tsf --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（exit=$LASTEXITCODE）" }
    } finally { Pop-Location }
    Trace-Script "dev-deploy: 构建完成"
} else {
    Trace-Script "dev-deploy: -SkipBuild，跳过构建"
}

# ---- 2. 产物检查 ----
if (-not (Test-Path $dllSrc)) {
    Trace-Script "dev-deploy: 错误，DLL 缺失 $dllSrc"
    Write-Host "错误：未找到 $dllSrc"
    Write-Host "请先执行：cargo build -p iuv-tsf --release（或去掉 -SkipBuild）"
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
  "candidate_prefix": false
}
'@
    [IO.File]::WriteAllText($configPath, $template, [Text.UTF8Encoding]::new($false))
    Trace-Script "dev-deploy: 生成默认配置 $configPath"
    Write-Host "已生成默认配置（可编辑注释后改设置）：$configPath"
}

# ---- 3. 复制（DLL 用热替换：未锁直接复制，锁了改名 .old 原位替换，零杀进程）----
try {
    Copy-Item $imedicSrc $dictDest -Force -ErrorAction Stop
    Trace-Script "dev-deploy: 词库复制成功 $dictDest"
} catch {
    Trace-Script "dev-deploy: 词库复制失败（$dictDest）"
    Write-Host "错误：词库复制失败：$dictDest"
    Write-Host "请关闭占用 %LOCALAPPDATA%\iuv 的进程后重跑。"
    exit 1
}
$r = Replace-InUseDll -Src $dllSrc -Dest $destDll
if (-not $r.Ok) { exit 1 }
if ($r.Renamed) {
    Write-Host "DLL 被占用，已用改名替换：老进程仍用旧 DLL，新进程将加载新 DLL（.old 将在注销/重启后自动清理）。"
} else {
    Trace-Script "dev-deploy: DLL 复制成功 $destDll"
}

# ---- 4. 注册（仅未注册时；DLL 路径不变，热替换无需重注册）----
if (-not (Test-Path $clsidKey) -or -not (Test-Path $tipKey)) {
    Trace-Script "dev-deploy: 开始 regsvr32 $destDll"
    Write-Host "正在注册 COM/TSF 服务..."
    & "$env:windir\System32\regsvr32.exe" /s $destDll
    Start-Sleep -Seconds 1
    if (-not (Test-Path $clsidKey) -or -not (Test-Path $tipKey)) {
        Trace-Script "dev-deploy: 注册失败（CLSID=$(Test-Path $clsidKey) TIP=$(Test-Path $tipKey)）"
        Write-Host "错误：注册失败。日志见 %TEMP%\iuv-script.log"
        exit 1
    }
    Trace-Script "dev-deploy: regsvr32 完成，CLSID=True TIP=True"
} else {
    Trace-Script "dev-deploy: 已注册，跳过 regsvr32"
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
