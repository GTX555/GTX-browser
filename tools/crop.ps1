Add-Type -AssemblyName System.Drawing

# Slices of the captured screenshots, scaled up so details are readable.
$slices = @(
    @{ In = "tabs-favicon";  X = 0;    Y = 0;  W = 980;  H = 120; Zoom = 1.5; Out = "c1-left" },
    @{ In = "tabs-favicon";  X = 1400; Y = 0;  W = 538;  H = 120; Zoom = 1.5; Out = "c1-right" },
    @{ In = "suggest";       X = 120;  Y = 4;  W = 620;  H = 54; Zoom = 3; Out = "addr-suggest" },
    @{ In = "tabs-favicon";  X = 120;  Y = 4;  W = 620;  H = 54; Zoom = 3; Out = "addr-tabs" },
    @{ In = "suggest";       X = 110;  Y = 40; W = 660;  H = 420; Zoom = 1.4; Out = "c2-popup" },
    @{ In = "suggest";       X = 1400; Y = 0;  W = 538;  H = 120; Zoom = 1.5; Out = "c2-right" },
    @{ In = "downloads-panel"; X = 1140; Y = 40; W = 700; H = 420; Zoom = 1.4; Out = "c3-popup" },
    @{ In = "downloads-panel"; X = 1400; Y = 0;  W = 538; H = 120; Zoom = 1.5; Out = "c3-right" }
)

foreach ($s in $slices) {
    $src = [System.Drawing.Bitmap]::FromFile("$PWD\target\$($s.In).png")
    $r = New-Object System.Drawing.Rectangle($s.X, $s.Y, $s.W, $s.H)
    $crop = $src.Clone($r, $src.PixelFormat)
    $src.Dispose()
    $zw = [int]($s.W * $s.Zoom)
    $zh = [int]($s.H * $s.Zoom)
    $big = New-Object System.Drawing.Bitmap($zw, $zh)
    $g = [System.Drawing.Graphics]::FromImage($big)
    $g.InterpolationMode = "NearestNeighbor"
    $g.PixelOffsetMode = "Half"
    $g.DrawImage($crop, 0, 0, $zw, $zh)
    $g.Dispose()
    $crop.Dispose()
    $big.Save("$PWD\target\$($s.Out).png", [System.Drawing.Imaging.ImageFormat]::Png)
    $big.Dispose()
    Write-Output ("{0} -> {1}x{2}" -f $s.Out, $zw, $zh)
}
