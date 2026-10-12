param(
    [string]$Exe = ".\target\debug\gtx-browser.exe",
    [string]$Out = ".\target\chrome.png",
    [int]$WaitMs = 5500,
    [string]$AppArgs = "--no-restore"
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class GtxCap {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] private struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] private struct POINT { public int X, Y; }
  public static int[] Window(IntPtr h) { RECT r; GetWindowRect(h, out r); return new[]{r.Left,r.Top,r.Right,r.Bottom}; }
  public static int[] Client(IntPtr h) {
    RECT c; GetClientRect(h, out c);
    POINT p = new POINT { X = c.Left, Y = c.Top };
    ClientToScreen(h, ref p);
    return new[]{p.X, p.Y, p.X + (c.Right - c.Left), p.Y + (c.Bottom - c.Top)};
  }
}
"@

[GtxCap]::SetProcessDPIAware() | Out-Null

$p = Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru
Start-Sleep -Milliseconds $WaitMs
$p.Refresh()
$h = $p.MainWindowHandle
if ($h -eq [IntPtr]::Zero) { Write-Output "no main window"; Stop-Process -Id $p.Id -Force -EA SilentlyContinue; exit 1 }

[GtxCap]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 700

$w = [GtxCap]::Window($h)
$c = [GtxCap]::Client($h)
$ww = $w[2] - $w[0]; $wh = $w[3] - $w[1]
$cw = $c[2] - $c[0]; $ch = $c[3] - $c[1]
Write-Output ("window {0}x{1} at {2},{3} | client {4}x{5} at {6},{7} (inset L{8} T{9} R{10} B{11})" -f `
    $ww,$wh,$w[0],$w[1],$cw,$ch,$c[0],$c[1],($c[0]-$w[0]),($c[1]-$w[1]),($w[2]-$c[2]),($w[3]-$c[3]))

$bmp = New-Object System.Drawing.Bitmap($ww, $wh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($w[0], $w[1], 0, 0, (New-Object System.Drawing.Size($ww, $wh)))
$g.Dispose()

function Probe($x, $y) {
    $p = $bmp.GetPixel($x, $y)
    return "({0},{1})=#{2}" -f $x, $y, $p.R.ToString("x2") + $p.G.ToString("x2") + $p.B.ToString("x2")
}
Write-Output ("probes: " + ((Probe 1 1), (Probe 30 4), (Probe 300 4), (Probe 300 30), (Probe 700 30), (Probe 300 60), (Probe 60 120) -join " "))

$full = [System.IO.Path]::GetFullPath($Out)
$bmp.Save($full, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "saved -> $full"

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
