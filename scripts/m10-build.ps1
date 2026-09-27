# M10 测试构建（薄客户端 + 引擎服务端，49 号）：四车道并行。
# 无需管理员权限。
#
# 车道：x64-tsf（target\）∥ x86-tsf（target\i686-…）∥ daemon（target-daemon\，--features dev）
#       ∥ server（target-server\，iuv-server.exe 引擎服务进程）。
# 各车道独立 target 目录互不持锁，并行耗时 = 最长单链；与 dev-deploy/install 同款约定。
#
# 产物：
#   target\release\iuv_tsf.dll
#   target\i686-pc-windows-msvc\release\iuv_tsf.dll
#   target-daemon\release\iuv-daemon.exe
#   target-server\release\iuv-server.exe
#requires -Version 5.1

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot

Write-Host "M10 四车道并行构建（x64 TSF / x86 TSF / iuv-daemon / iuv-server）..."
Push-Location $repoRoot
try {
    $buildSpecs = @(
        @{ Name = 'x64-tsf'; Env = @{}; CargoArgs = @('build', '-p', 'iuv-tsf', '--release') },
        @{ Name = 'x86-tsf'; Env = @{};
           CargoArgs = @('build', '-p', 'iuv-tsf', '--release', '--target', 'i686-pc-windows-msvc') },
        @{ Name = 'daemon';  Env = @{ CARGO_TARGET_DIR = (Join-Path $repoRoot 'target-daemon') };
           CargoArgs = @('build', '-p', 'iuv-daemon', '--release', '--features', 'dev') },
        @{ Name = 'server';  Env = @{ CARGO_TARGET_DIR = (Join-Path $repoRoot 'target-server') };
           CargoArgs = @('build', '-p', 'iuv-server', '--release') }
    )
    $jobs = foreach ($spec in $buildSpecs) {
        Start-Job -Name "iuv-build-$($spec.Name)" -ScriptBlock {
            param($dir, $envMap, $cargoArgs)
            Set-Location $dir
            foreach ($k in $envMap.Keys) { Set-Item -Path "env:$k" -Value $envMap[$k] }
            $out = & cargo @cargoArgs 2>&1
            [pscustomobject]@{ Code = $LASTEXITCODE; Output = ($out | Out-String) }
        } -ArgumentList $repoRoot, $spec.Env, $spec.CargoArgs
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
            Write-Host "车道完成：$name"
        }
    }
    if ($failed) { throw "cargo build 失败（详见上方各车道输出）" }
} finally {
    Get-Job -Name 'iuv-build-*' -ErrorAction SilentlyContinue | Remove-Job -Force -ErrorAction SilentlyContinue
    Pop-Location
}

# ---- 产物自检 ----
$artifacts = @(
    (Join-Path $repoRoot "target\release\iuv_tsf.dll"),
    (Join-Path $repoRoot "target\i686-pc-windows-msvc\release\iuv_tsf.dll"),
    (Join-Path $repoRoot "target-daemon\release\iuv-daemon.exe"),
    (Join-Path $repoRoot "target-server\release\iuv-server.exe")
)
$missing = @($artifacts | Where-Object { -not (Test-Path $_) })
if ($missing.Count -gt 0) {
    # throw 而非 exit：被 m10-deploy 调用时能被 try/catch 捕获（exit 只退子脚本，
    # 调用方的 LASTEXITCODE 检查不可靠——见 m10-deploy 同款注释）。
    throw "以下构建产物缺失：$($missing -join '; ')"
}
Write-Host ""
Write-Host "M10 构建完成（四产物齐全）。下一步：scripts\m10-deploy.ps1（需管理员）"
