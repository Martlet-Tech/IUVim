# 注销 Input IME 文本服务（需管理员权限）。
# 契约 13 任务书 §3.8：regsvr32 /u → 删除文件 → 重启 ctfmon。
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

$destDir   = Join-Path $env:ProgramData "InputIME"
$dllPath   = Join-Path $destDir "input_ime_tsf.dll"

if (Test-Path $dllPath) {
    Write-Host "正在注销 COM/TSF 服务..."
    regsvr32 /s /u $dllPath
    if ($LASTEXITCODE -ne 0) {
        Write-Host "错误：regsvr32 注销失败（exit=$LASTEXITCODE）。"
        exit $LASTEXITCODE
    }
} else {
    Write-Host "未找到 $dllPath，跳过 regsvr32 /u。"
}

Write-Host "正在删除安装文件..."
Remove-Item -Path $destDir -Recurse -Force -ErrorAction SilentlyContinue

Write-Host "正在重启 ctfmon..."
taskkill /f /im ctfmon.exe 2>$null | Out-Null
Start-Sleep -Milliseconds 300
Start-Process ctfmon.exe

Write-Host "注销完成。"
