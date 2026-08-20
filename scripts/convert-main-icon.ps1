# 一次性资源转换：`assets/main.png`（输入法主 logo）→ `res/icon.ico`（语言栏主图标 / DLL 文件图标）。
#
# 语义：TSF 语言栏「中英按钮右侧主图标」= TIP 注册的 `ulIconIndex=0` → DLL 图标组 ID "1"
# = `res/icon.ico`（build.rs `set_icon` 编译期内嵌）。本脚本把 main.png 转成多尺寸 32bpp
# .ico 覆盖该文件；转换是一次性的，之后构建在编译期完成、运行时零开销（LoadImageW + LR_SHARED）。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\convert-main-icon.ps1
#
# 产物：经典 32bpp DIB 条目（BITMAPINFOHEADER + BGRA XOR 位图 + 全零 AND 掩码），
# `LoadImageW` 全兼容；尺寸 16/24/32/48/64/128/256（0=256），语言栏取 16、指示器取 32~48、
# 高 DPI 自动取大图。
# 仅 Windows（依赖 .NET System.Drawing）；需要 PowerShell 5.1+（内置 GDI+）。

$ErrorActionPreference = "Stop"

$source = Join-Path $PSScriptRoot "..\assets\main.png"
$out    = Join-Path $PSScriptRoot "..\platforms\windows\iuv-tsf\res\icon.ico"

if (-not (Test-Path $source)) {
    throw "源图不存在：$source"
}

Add-Type -AssemblyName System.Drawing

$src = [System.Drawing.Bitmap]::FromFile((Resolve-Path $source))
try {
    $sizes = @(16, 24, 32, 48, 64, 128, 256)

    # 1. 逐个尺寸缩小 + 提取 DIB 图像（BITMAPINFOHEADER + XOR(BGRA 自底向上) + AND 掩码全零）。
    #    Format32bppArgb + CompositingMode.SourceCopy 保证 alpha 通道保留（logo 有透明圆角）。
    $images = @()
    foreach ($s in $sizes) {
        $dest = New-Object System.Drawing.Bitmap($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $g = [System.Drawing.Graphics]::FromImage($dest)
            try {
                $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
                $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
                $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
                $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $g.DrawImage($src, 0, 0, $s, $s)
            } finally { $g.Dispose() }

            # XOR 数据：自底向上（ICO 存储首行为最底行）。
            $xor = New-Object byte[] ($s * $s * 4)
            for ($y = 0; $y -lt $s; $y++) {
                $dstRow = $s - 1 - $y
                for ($x = 0; $x -lt $s; $x++) {
                    $c = $dest.GetPixel($x, $y)
                    $i = ($dstRow * $s + $x) * 4
                    $xor[$i]     = $c.B
                    $xor[$i + 1] = $c.G
                    $xor[$i + 2] = $c.R
                    $xor[$i + 3] = $c.A
                }
            }

            # BITMAPINFOHEADER（biHeight = h*2 表示 XOR+AND 两段）。
            $bh = New-Object byte[] 40
            [BitConverter]::GetBytes([int32]40).CopyTo($bh, 0)          # biSize
            [BitConverter]::GetBytes([int32]$s).CopyTo($bh, 4)          # biWidth
            [BitConverter]::GetBytes([int32]($s * 2)).CopyTo($bh, 8)    # biHeight
            [BitConverter]::GetBytes([int16]1).CopyTo($bh, 12)          # biPlanes
            [BitConverter]::GetBytes([int16]32).CopyTo($bh, 14)         # biBitCount
            [BitConverter]::GetBytes([int32]0).CopyTo($bh, 16)          # biCompression = BI_RGB
            [BitConverter]::GetBytes([int32]($s * $s * 4)).CopyTo($bh, 20) # biSizeImage

            # AND 掩码：32bpp 走 alpha 通道，掩码全零（不透出区域）即可。
            $andLen = $s * [math]::Ceiling($s / 32) * 4
            $and = New-Object byte[] $andLen

            $img = New-Object byte[] ($bh.Length + $xor.Length + $and.Length)
            $bh.CopyTo($img, 0)
            $xor.CopyTo($img, $bh.Length)
            $and.CopyTo($img, $bh.Length + $xor.Length)

            $images += ,@{
                w   = $s
                len = $img.Length
                img = $img
            }
        } finally { $dest.Dispose() }
    }

    # 2. 组装 ICO：ICONDIR + ICONDIRENTRY × N + 图像数据。
    $count = $images.Count
    $headerLen = 6 + 16 * $count
    $fs = [System.IO.File]::Create($out)
    try {
        $bw = New-Object System.IO.BinaryWriter($fs)

        $bw.Write([uint16]0)                 # reserved
        $bw.Write([uint16]1)                 # type = icon
        $bw.Write([uint16]$count)            # count

        $offset = $headerLen
        foreach ($im in $images) {
            $w = $im.w
            $bw.Write([byte]($(if ($w -ge 256) { 0 } else { $w })))   # width（256 → 0）
            $bw.Write([byte]($(if ($w -ge 256) { 0 } else { $w })))   # height
            $bw.Write([byte]0)               # colorCount
            $bw.Write([byte]0)               # reserved
            $bw.Write([uint16]1)             # planes
            $bw.Write([uint16]32)            # bitCount
            $bw.Write([uint32]$im.len)       # bytesInRes
            $bw.Write([uint32]$offset)       # imageOffset
            $offset += $im.len
        }
        foreach ($im in $images) {
            $bw.Write($im.img)
        }
        $bw.Flush()
    } finally {
        $fs.Dispose()
    }

    $size = (Get-Item $out).Length
    Write-Host "OK：$out（$size 字节，$count 个尺寸：$($sizes -join ',')）"
} finally {
    $src.Dispose()
}
