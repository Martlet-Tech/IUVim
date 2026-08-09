# 注册 Input IME 文本服务（需管理员权限）。
# 契约 13 任务书 §3.8：复制 DLL 与词库到 %ProgramData%\InputIME → regsvr32 → 重启 ctfmon。
#requires -Version 5.1

$ErrorActionPreference = "Stop"

function Assert-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Write-Host "错误：需要管理员权限。请右键以管理员身份运行。"
        exit 1
    }
}

Assert-Admin

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$dllSrc    = Join-Path $scriptDir "..\target\release\input_ime_tsf.dll"
$imedicSrc = Join-Path $scriptDir "..\data\input.imedic"
$destDir   = Join-Path $env:ProgramData "InputIME"

if (-not (Test-Path $dllSrc)) {
    Write-Host "错误：未找到 $dllSrc"
    Write-Host "请先执行：cargo build -p ime-tsf --release"
    exit 1
}

New-Item -ItemType Directory -Force -Path $destDir | Out-Null

Copy-Item $dllSrc (Join-Path $destDir "input_ime_tsf.dll") -Force
Write-Host "已复制 DLL → $destDir"

if (Test-Path $imedicSrc) {
    Copy-Item $imedicSrc (Join-Path $destDir "input.imedic") -Force
    Write-Host "已复制词库 → $destDir"
} else {
    Write-Host "警告：未找到词库 $imedicSrc（输入法将进入透明模式，全部按键放行）。"
    Write-Host "请先执行：scripts\download-dict.ps1 与 dictc 编译（见 docs/plan/20-assembly.md §3）。"
}

Write-Host "正在注册 COM/TSF 服务..."
regsvr32 /s (Join-Path $destDir "input_ime_tsf.dll")
if ($LASTEXITCODE -ne 0) {
    Write-Host "错误：regsvr32 注册失败（exit=$LASTEXITCODE）。请查看 %TEMP%\input-ime-tsf.log。"
    exit $LASTEXITCODE
}

Write-Host "正在重启 ctfmon 以加载新输入法..."
taskkill /f /im ctfmon.exe 2>$null | Out-Null
Start-Sleep -Milliseconds 300
Start-Process ctfmon.exe

Write-Host ""
Write-Host "注册完成。下一步（W2 手测）："
Write-Host "  1. 打开 Windows 设置 → 时间和语言 → 语言 → 中文 → 键盘，添加/切换到 'Input IME'"
Write-Host "  2. 在记事本中输入拼音验证（预编辑文本 + 候选窗）"
