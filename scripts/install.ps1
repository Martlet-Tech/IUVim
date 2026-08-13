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
$imedicSrc  = Join-Path $repoRoot "data\iuv.imedic"
$dictsDir   = Join-Path $repoRoot "data\rime-frost\cn_dicts"
$destDir    = Join-Path $env:ProgramFiles "iuv"              # DLL：程序文件位置
$destDll    = Join-Path $destDir "iuv_tsf.dll"
$dictDir    = Join-Path $env:LOCALAPPDATA "iuv"              # 词库：用户级数据
$dictDest   = Join-Path $dictDir "iuv.imedic"
$clsidKey   = 'Registry::HKEY_CLASSES_ROOT\CLSID\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'
$tipKey     = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\{C69735F1-BAB1-458B-89FC-099ABA877ECB}'

# ---- 1. 构建检查 ----
if (-not (Test-Path $dllSrc)) {
    Trace-Script "install: 错误，构建产物缺失 $dllSrc"
    Write-Host "错误：未找到 $dllSrc"
    Write-Host "请先执行：cargo build -p iuv-tsf --release"
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

# ---- 3. 安装 DLL（被占用时登记延迟替换）----
# 先清除指向安装目录的陈旧 pending op（旧版无去重卸载可能留下"重启删目录"条目，
# 不清理的话下次重启会删掉刚装的目录）。
$cleared = Clear-PendingOp -Source $destDir
Trace-Script ("install: 陈旧 pending op 清理=" + $cleared)
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$queuedReplace = $false
try {
    Copy-Item $dllSrc $destDll -Force -ErrorAction Stop
    Trace-Script "install: DLL 复制成功 $destDll"
    Write-Host "已安装 DLL：$destDll"
} catch {
    # 目标被占用：复制到临时名，双保险登记延迟替换（注销/重启后自动生效）。
    $tmp = Join-Path $destDir "iuv_tsf.new.dll"
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

# ---- 4. 安装词库（用户级数据，不锁定）----
New-Item -ItemType Directory -Force -Path $dictDir | Out-Null
Copy-Item $imedicSrc $dictDest -Force
Trace-Script "install: 词库复制 $dictDest"
Write-Host "已安装词库：$dictDest"

# ---- 5. 注册（未注册、DLL 路径或显示名变化时重注册；否则跳过）----
$registeredPath = $null
$registeredDesc = $null
if (Test-Path "$clsidKey\InprocServer32") {
    $registeredPath = (Get-ItemProperty -Path "$clsidKey\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
    $registeredDesc = (Get-ItemProperty -Path $clsidKey -ErrorAction SilentlyContinue).'(default)'
}
# 显示名比较必须区分大小写（-cne；-ne 大小写不敏感会把 iuv/IUV 判为相等）。
if (-not (Test-Path $clsidKey) -or -not (Test-Path $tipKey) -or $registeredPath -ne $destDll -or $registeredDesc -cne $profileDesc) {
    Trace-Script "install: 开始 regsvr32 $destDll"
    Write-Host "正在注册 COM/TSF 服务..."
    & "$env:windir\System32\regsvr32.exe" /s $destDll
    Start-Sleep -Seconds 1
    Trace-Script ("install: regsvr32 返回，CLSID=" + (Test-Path $clsidKey) + " TIP=" + (Test-Path $tipKey))
    # 不依赖 regsvr32 退出码（$LASTEXITCODE 可能为 $null）；以注册表是否写入为准。
    if (-not (Test-Path $clsidKey) -or -not (Test-Path $tipKey)) {
        Write-Host "错误：注册失败（CLSID=$(Test-Path $clsidKey) TIP=$(Test-Path $tipKey)）。日志见 %TEMP%\iuv-tsf.log"
        exit 1
    }
} else {
    Trace-Script "install: 已注册，跳过 regsvr32"
    Write-Host "已注册（跳过 regsvr32）。"
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
