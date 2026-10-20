# 出安装包：三车道构建 → 汇集产物到 build\pkg → Inno Setup 编译 → dist\iuv-setup-<ver>.exe
#
# 用法：
#   scripts\package.ps1              # 构建 + 出包
#   scripts\package.ps1 -SkipBuild   # 用现有 target 产物直接出包（改 .iss 后快速验证）
#
# 前置：Rust 工具链 + i686-pc-windows-msvc target + Inno Setup 6（ISCC.exe）。
# 本脚本**不需要管理员权限**（编译安装包不需要，跑安装包才需要）。
# 词库缺失会自动走 Ensure-Imedic / Ensure-Opencc（下载 + dictc 编译，几分钟）。
#
# 产物：dist\iuv-setup-<version>.exe（单文件，约 40-50 MB，含词库）
#requires -Version 5.1

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'iuv-common.ps1')

$p = Get-IuvPaths
$root = $p.RepoRoot
$pkgDir = Join-Path $root 'build\pkg'
$distDir = Join-Path $root 'dist'

# ---- 1. 版本号：唯一真相源 = Cargo.toml 的 [workspace.package] version ----
# 直接解析文本而不问 cargo：出包要能在任何机器上跑，不依赖 cargo metadata 的耗时。
# **必须显式按 UTF-8 读**：Windows PowerShell 5.1 的 Get-Content 默认按系统 ANSI
# （简中 = GBK）解码，Cargo.toml 是无 BOM 的 UTF-8，中文注释会被解成乱码并让
# 后续正则失配（2026-10-20 实际踩中：`-raw` 下多字节错位，`^version` 匹配不上）。
$cargoToml = [System.IO.File]::ReadAllText((Join-Path $root 'Cargo.toml'), [System.Text.Encoding]::UTF8)
if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
    Write-Host "错误：无法从 Cargo.toml 的 [workspace.package] 解析版本号"
    exit 1
}
$version = $Matches[1]
Write-Host "版本：$version"

# ---- 2. 构建（缺产物或未 -SkipBuild 时）----
if (-not $SkipBuild) {
    try { Build-All } catch { Write-Host "错误：构建失败：$_"; exit 1 }
} else {
    $missing = @($p.DllSrc, $p.DllSrc32, $p.ServerSrc | Where-Object { -not (Test-Path $_) })
    if ($missing.Count -gt 0) {
        Write-Host "错误：-SkipBuild 但产物缺失：$($missing -join '; ')"
        exit 1
    }
    Write-Host "-SkipBuild：使用现有构建产物"
}

# ---- 3. 词库链（缺失则下载 + 编译）----
try {
    Ensure-Imedic
    Ensure-Opencc
} catch {
    Write-Host "错误：词库准备失败：$_"
    exit 1
}

# ---- 4. 汇集到 build\pkg（Inno 只认一个目录，不跨 target 树取文件）----
if (Test-Path $pkgDir) { Remove-Item $pkgDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $pkgDir | Out-Null
$pairs = @(
    @{ Src = $p.DllSrc;     Dst = 'iuv_tsf.dll' },
    @{ Src = $p.DllSrc32;   Dst = 'iuv_tsf_x86.dll' },
    @{ Src = $p.ServerSrc;  Dst = 'iuv-server.exe' },
    @{ Src = $p.ImedicSrc;  Dst = 'iuv.imedic' },
    @{ Src = $p.OpenccSrc;  Dst = 'iuv.opencc' }
)
foreach ($pair in $pairs) {
    if (-not (Test-Path $pair.Src)) { Write-Host "错误：缺少 $($pair.Src)"; exit 1 }
    Copy-Item $pair.Src (Join-Path $pkgDir $pair.Dst) -Force
    $mb = [math]::Round((Get-Item (Join-Path $pkgDir $pair.Dst)).Length / 1MB, 1)
    Write-Host ("  汇集 {0,-18} {1,6} MB" -f $pair.Dst, $mb)
}

# ---- 5. 找 ISCC.exe ----
# winget 默认装到用户目录（%LOCALAPPDATA%\Programs\Inno Setup 6），不是 Program Files。
$isccCandidates = @(@(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 5\ISCC.exe"
) | Where-Object { Test-Path $_ })
# 外层 @() 不可省：只剩一个候选时 Where-Object 返回标量字符串，
# 后续 $isccCandidates[0] 会索引到第一个**字符**（"C"）。
if (-not $isccCandidates) {
    Write-Host ""
    Write-Host "错误：未找到 Inno Setup 6（ISCC.exe）。安装方式任选："
    Write-Host "  winget install --id JRSoftware.InnoSetup -e"
    Write-Host "  choco install innosetup -y"
    exit 1
}
$iscc = $isccCandidates[0]
Write-Host "ISCC：$iscc"

# ---- 6. 编译安装包 ----
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
$iss = Join-Path $root 'installer\iuv-setup.iss'
$out = & $iscc "/DAppVersion=$version" $iss 2>&1
$code = $LASTEXITCODE
$out | Select-Object -Last 25
if ($code -ne 0) {
    Write-Host "错误：ISCC 编译失败（退出码 $code）"
    exit 1
}

$setup = Join-Path $distDir "iuv-setup-$version.exe"
if (-not (Test-Path $setup)) {
    Write-Host "错误：ISCC 返回成功但找不到 $setup"
    exit 1
}
$size = [math]::Round((Get-Item $setup).Length / 1MB, 1)
Write-Host ""
Write-Host "出包完成：$setup（$size MB）"
Write-Host "安装（管理员）：双击该文件。卸载：设置 → 应用 → IUV 输入法，或安装目录内 unins000.exe。"
