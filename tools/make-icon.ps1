# Generates assets/gtx.ico (16/32/48 px, hand-written ICO container).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools\make-icon.ps1
Add-Type -AssemblyName System.Drawing

function Draw-Gtx([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.TextRenderingHint = 'AntiAliasGridFit'
    $g.Clear([System.Drawing.Color]::Transparent)

    # Dark rounded square.
    $rad = [int][Math]::Max(2, $s * 0.22)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc(0, 0, 2 * $rad, 2 * $rad, 180, 90)
    $path.AddArc($s - 2 * $rad, 0, 2 * $rad, 2 * $rad, 270, 90)
    $path.AddArc($s - 2 * $rad, $s - 2 * $rad, 2 * $rad, 2 * $rad, 0, 90)
    $path.AddArc(0, $s - 2 * $rad, 2 * $rad, 2 * $rad, 90, 90)
    $path.CloseFigure()
    $rect = New-Object System.Drawing.Rectangle 0, 0, $s, $s
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, `
        ([System.Drawing.Color]::FromArgb(255, 26, 29, 35)), `
        ([System.Drawing.Color]::FromArgb(255, 58, 63, 72)), 45
    $g.FillPath($brush, $path)

    # GTX in performance green (single G at 16 px so it stays readable).
    $pt = [Math]::Round($s * $(if ($s -le 16) { 0.62 } else { 0.34 }), 1)
    $font = New-Object System.Drawing.Font 'Segoe UI', $pt, `
        ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $green = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0, 230, 118))
    $sf = New-Object System.Drawing.StringFormat
    $sf.Alignment = 'Center'
    $sf.LineAlignment = 'Center'
    $text = if ($s -le 16) { 'G' } else { 'GTX' }
    $g.DrawString($text, $font, $green, (New-Object System.Drawing.RectangleF 0, 0, $s, $s), $sf)
    $g.Dispose()
    return $bmp
}

function Write-Ico([string]$path, $bitmaps) {
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    $bw.Write([uint16]0)              # reserved
    $bw.Write([uint16]1)              # type: icon
    $bw.Write([uint16]$bitmaps.Count) # count

    $images = @()
    foreach ($bmp in $bitmaps) {
        $s = $bmp.Width
        $rect = New-Object System.Drawing.Rectangle 0, 0, $s, $s
        $data = $bmp.LockBits($rect, 'ReadOnly', 'Format32bppArgb')
        $len = $data.Stride * $s
        $top = New-Object byte[] $len
        [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $top, 0, $len)
        $bmp.UnlockBits($data)

        # ICO stores rows bottom-up; copy flipping each row.
        $xor = New-Object byte[] $len
        for ($y = 0; $y -lt $s; $y++) {
            [Array]::Copy($top, ($s - 1 - $y) * $data.Stride, $xor, $y * $data.Stride, $s * 4)
        }
        # AND mask all-zero: alpha channel decides transparency.
        $maskStride = ((($s + 31) -shr 5) -shl 2)
        $and = New-Object byte[] ($maskStride * $s)

        $hdr = New-Object System.IO.MemoryStream
        $hw = New-Object System.IO.BinaryWriter $hdr
        $hw.Write([uint32]40)             # BITMAPINFOHEADER size
        $hw.Write([int32]$s)              # width
        $hw.Write([int32](2 * $s))        # height (xor + and)
        $hw.Write([uint16]1)              # planes
        $hw.Write([uint16]32)             # bpp
        $hw.Write([uint32]0)              # no compression
        $hw.Write([uint32]($s * $s * 4))  # image size ( tolerated 0, but be exact-ish )
        $hw.Write([int32]0)
        $hw.Write([int32]0)
        $hw.Write([uint32]0)
        $hw.Write([uint32]0)
        $images += ,(@($hdr.ToArray(), $xor, $and))
    }

    # Header is 6 bytes + 16 per entry; compute offsets, then write bodies.
    $offset = 6 + 16 * $images.Count
    for ($i = 0; $i -lt $images.Count; $i++) {
        $s = $bitmaps[$i].Width
        $img = $images[$i]
        $bodyLen = $img[0].Length + $img[1].Length + $img[2].Length
        $bw.Write([byte]$(if ($s -ge 256) { 0 } else { $s })) # width
        $bw.Write([byte]$(if ($s -ge 256) { 0 } else { $s })) # height
        $bw.Write([byte]0)          # colors in palette
        $bw.Write([byte]0)          # reserved
        $bw.Write([uint16]1)        # color planes
        $bw.Write([uint16]32)       # bpp
        $bw.Write([uint32]$bodyLen) # size of image data
        $bw.Write([uint32]$offset)  # offset
        $offset += $bodyLen
    }
    foreach ($img in $images) {
        $bw.Write($img[0]); $bw.Write($img[1]); $bw.Write($img[2])
    }
    [System.IO.File]::WriteAllBytes($path, $ms.ToArray())
    $bw.Dispose()
}

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'assets\gtx.ico'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $out) | Out-Null
$bitmaps = @((Draw-Gtx 16), (Draw-Gtx 32), (Draw-Gtx 48))
Write-Ico $out $bitmaps
Write-Output "wrote $out ($([System.IO.File]::ReadAllBytes($out).Length) bytes)"
