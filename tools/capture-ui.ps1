param(
    [string]$Exe = ".\target\debug\gtx-browser.exe",
    [string]$Out = ".\target\ui.png",
    [string]$AppArgs = "--no-restore",
    [int]$WaitMs = 5000,
    [string]$Keys = "",
    [string]$Click = "",
    [int]$ActionWaitMs = 1400
)

# Captures the browser after optional interaction:
#   -Click "x,y"   clicks that client coordinate (mouse down/up)
#   -Keys "^lduck" sends Ctrl+L then types, so the suggestion panel opens
# ASCII only: PowerShell 5.1 reads this BOM-less.

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class GtxUi {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  [StructLayout(LayoutKind.Sequential)] private struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] private struct POINT { public int X, Y; }
  public static int[] Window(IntPtr h) { RECT r; GetWindowRect(h, out r); return new[]{r.Left,r.Top,r.Right,r.Bottom}; }
  public static int[] Client(IntPtr h) {
    RECT c; GetClientRect(h, out c);
    POINT p = new POINT { X = c.Left, Y = c.Top };
    ClientToScreen(h, ref p);
    return new[]{p.X, p.Y, p.X + (c.Right - c.Left), p.Y + (c.Bottom - c.Top)};
  }
  public static void ClickAt(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(200);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero); // left down
    mouse_event(4, 0, 0, 0, UIntPtr.Zero); // left up
  }
}
"@

[GtxUi]::SetProcessDPIAware() | Out-Null

$p = Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru
Start-Sleep -Milliseconds $WaitMs
$p.Refresh()
$h = $p.MainWindowHandle
if ($h -eq [IntPtr]::Zero) {
    Write-Output "no main window"
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    exit 1
}

[GtxUi]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 700
$c = [GtxUi]::Client($h)
Write-Output ("client at {0},{1} .. {2},{3}" -f $c[0], $c[1], $c[2], $c[3])

if ($Click -ne "") {
    $px = 0
    $py = 0
    if ($Click -eq "dl") {
        # The downloads button: right margin = 3 caption + kebab + gap, half a
        # button in, vertically centred in the row (96-dpi units x 1.25).
        $px = ($c[2] - $c[0]) - 243
        $py = 28
    } else {
        $xy = $Click -split ","
        $px = [int]$xy[0]
        $py = [int]$xy[1]
    }
    $sx = $c[0] + $px
    $sy = $c[1] + $py
    Write-Output ("click client {0},{1} -> screen {2},{3}" -f $px, $py, $sx, $sy)
    [GtxUi]::ClickAt($sx, $sy)
    Start-Sleep -Milliseconds $ActionWaitMs
}

if ($Keys -ne "") {
    [System.Windows.Forms.SendKeys]::SendWait("^l")
    Start-Sleep -Milliseconds 400
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
    Start-Sleep -Milliseconds $ActionWaitMs
}

$w = [GtxUi]::Window($h)
$ww = $w[2] - $w[0]
$wh = $w[3] - $w[1]
$bmp = New-Object System.Drawing.Bitmap($ww, $wh)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($w[0], $w[1], 0, 0, (New-Object System.Drawing.Size($ww, $wh)))
$g.Dispose()

$full = [System.IO.Path]::GetFullPath($Out)
$bmp.Save($full, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "saved -> $full"

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
