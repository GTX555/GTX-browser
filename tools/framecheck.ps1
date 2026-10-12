param(
    [string]$Exe = ".\target\debug\gtx-browser.exe",
    [int]$WaitMs = 5000,
    [string]$AppArgs = "--no-restore"
)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class GtxFrame {
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr h, ref POINT p);
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

$p = Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru
Start-Sleep -Milliseconds $WaitMs
$p.Refresh()
$h = $p.MainWindowHandle
if ($h -eq [IntPtr]::Zero) { Write-Output "no main window"; Stop-Process -Id $p.Id -Force -EA SilentlyContinue; exit 1 }

$w = [GtxFrame]::Window($h)
$c = [GtxFrame]::Client($h)
Write-Output ("window : {0},{1} .. {2},{3}  ({4}x{5})" -f $w[0],$w[1],$w[2],$w[3],($w[2]-$w[0]),($w[3]-$w[1]))
Write-Output ("client : {0},{1} .. {2},{3}  ({4}x{5})" -f $c[0],$c[1],$c[2],$c[3],($c[2]-$c[0]),($c[3]-$c[1]))
Write-Output ("inset  : left={0} top={1} right={2} bottom={3}" -f ($c[0]-$w[0]),($c[1]-$w[1]),($w[2]-$c[2]),($w[3]-$c[3]))
Write-Output ("title   : {0}" -f $p.MainWindowTitle)

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
