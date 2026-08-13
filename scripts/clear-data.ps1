# 清除 iuv 输入法 用户数据（词频/学习记录），保留输入法本体与注册。
# 默认无需管理员；加 -Dict 同时删除部署词库（输入法回到透明模式，此时需提权）。
# 用法：scripts\clear-data.ps1            # 仅清用户数据（%LOCALAPPDATA%\iuv）
#       scripts\clear-data.ps1 -Dict     # 用户数据 + 部署词库
#requires -Version 5.1

param(
    [switch]$Dict
)

$ErrorActionPreference = "Stop"

# 用户数据目录：M2 起的学习词频/用户词库/配置（当前版本尚无产出，幂等清理）。
$userDataDir = Join-Path $env:LOCALAPPDATA "iuv"

if (Test-Path $userDataDir) {
    Remove-Item $userDataDir -Recurse -Force
    Write-Host "已清除用户数据：$userDataDir"
} else {
    Write-Host "用户数据目录不存在（无数据可清）：$userDataDir"
}

if ($Dict) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    $isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isAdmin) {
        Write-Host "删除部署词库需要管理员权限，正在弹出 UAC 提权窗口..."
        $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$($MyInvocation.MyCommand.Path)`"", "-Dict")
        Start-Process powershell -Verb RunAs -Wait -ArgumentList $argList
        if (-not $?) { Write-Host "提权被取消或失败。"; exit 1 }
        exit 0
    }
    $dictFile = Join-Path $env:LOCALAPPDATA "iuv\iuv.imedic"
    if (Test-Path $dictFile) {
        $removed = $false
        for ($i = 0; $i -lt 5; $i++) {
            Remove-Item $dictFile -Force -ErrorAction SilentlyContinue
            if (-not (Test-Path $dictFile)) { $removed = $true; break }
            Start-Sleep -Seconds 1
        }
        if ($removed) {
            Write-Host "已删除部署词库（输入法将进入透明模式）：$dictFile"
        } else {
            Write-Host "警告：无法删除 $dictFile（可能仍被占用），请稍后重试。"
        }
    } else {
        Write-Host "部署词库不存在：$dictFile"
    }
}

Write-Host "完成。"
