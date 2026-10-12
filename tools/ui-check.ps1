param(
    [string]$Exe = ".\target\debug\gtx-browser.exe",
    [string]$AppArgs = "--no-restore",
    [int]$WaitMs = 6000
)

# Verifies the UI features without guessing at pixels:
#   1. typing into the address bar really lands in the EDIT (reads its text)
#   2. the suggestion panel is a visible GtxPopup window owned by the app
#   3. clicking the downloads button opens the same popup, wider box
#   4. row icons sit in each panel's gutter, and rows highlight on hover
#   5. clicking the omnibox selects all of its text
#   6. a fresh tab hands over the address bar (focus lands on the EDIT)
#   7. the loading bar runs along the omnibox during a navigation
#   8. "open the downloads folder" really shows the folder in Explorer

#   9. the settings page hydrates (the footer leaves "Loading...")
#  10. flipping a switch really rewrites config.toml, and back again
#  11. a second launch forwards its link - still exactly one window
#  12. the history page paints the sites' own favicons
# ASCII only: PowerShell 5.1 reads this BOM-less.

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class GtxCheck {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  // Foreground lock: without the attach trick SetForegroundWindow can be
  // silently denied, and SendKeys would then type into whatever is on top.
  public static bool Focus(IntPtr h) {
    uint pid;
    uint target = GetWindowThreadProcessId(h, out pid);
    uint me = GetCurrentThreadId();
    bool attached = AttachThreadInput(me, target, true);
    bool ok = SetForegroundWindow(h);
    if (attached) AttachThreadInput(me, target, false);
    return ok;
  }
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] private static extern int GetClassName(IntPtr h, StringBuilder s, int max);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, StringBuilder l);
  [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr h, EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] private static extern IntPtr GetWindowLongPtr(IntPtr h, int i);
  private delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] private struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] private struct POINT { public int X, Y; }

  public static int[] Client(IntPtr h) {
    RECT c; GetClientRect(h, out c);
    POINT p = new POINT { X = c.Left, Y = c.Top };
    ClientToScreen(h, ref p);
    return new[]{p.X, p.Y, c.Right - c.Left, c.Bottom - c.Top};
  }

  private static List<IntPtr> Kids(IntPtr root) {
    var list = new List<IntPtr>();
    EnumChildWindows(root, (h, l) => { list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }

  // The address EDIT is the only Edit child of the chrome window.
  public static string AddressText(IntPtr root) {
    foreach (var h in Kids(root)) {
      var sb = new StringBuilder(64);
      GetClassName(h, sb, 64);
      if (sb.ToString() == "Edit") {
        IntPtr len = SendMessage(h, 0x000E /* WM_GETTEXTLENGTH */, IntPtr.Zero, IntPtr.Zero);
        var buf = new StringBuilder((int)len + 1);
        SendMessage(h, 0x000D /* WM_GETTEXT */, (IntPtr)buf.Capacity, buf);
        return buf.ToString();
      }
    }
    return "<no edit>";
  }

  // Visible GtxPopup child, as {Left, Top, Right, Bottom} in screen pixels.
  public static int[] Popup(IntPtr root) {
    IntPtr h = PopupHwnd(root);
    if (h == IntPtr.Zero) return null;
    RECT r; GetWindowRect(h, out r);
    return new[]{r.Left, r.Top, r.Right, r.Bottom};
  }

  public static IntPtr PopupHwnd(IntPtr root) {
    // The panel is an owned TOP-LEVEL window (menu style), not a child:
    // WebView2 composites its content above any sibling child window, so it
    // must live outside the main window's child list. Enumerate top-levels.
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      var sb = new StringBuilder(64);
      GetClassName(h, sb, 64);
      if (sb.ToString() == "GtxPopup" && IsWindowVisible(h)) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }

  // Renders the popup's own surface into hdc (PW_RENDERFULLCONTENT): this is
  // what the window contains, regardless of what is stacked above it.
  public static bool PrintPopup(IntPtr root, IntPtr hdc) {
    IntPtr h = PopupHwnd(root);
    if (h == IntPtr.Zero) return false;
    return PrintWindow(h, hdc, 2);
  }
  [DllImport("user32.dll")] private static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);

  public static void ClickAt(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(250);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }

  // The address EDIT child of the main window (a child of the chrome row).
  public static IntPtr EditHwnd(IntPtr root) {
    foreach (var h in Kids(root)) {
      var sb = new StringBuilder(64);
      GetClassName(h, sb, 64);
      if (sb.ToString() == "Edit") return h;
    }
    return IntPtr.Zero;
  }

  [DllImport("user32.dll")] public static extern IntPtr GetFocus();
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

  // GetFocus() only answers for the calling thread, so the app's input
  // thread has to be attached first or this always comes back as our own.
  public static IntPtr FocusedChild(IntPtr root) {
    uint pid;
    uint target = GetWindowThreadProcessId(root, out pid);
    uint me = GetCurrentThreadId();
    bool attached = AttachThreadInput(me, target, true);
    IntPtr f = GetFocus();
    if (attached) AttachThreadInput(me, target, false);
    return f;
  }

  // EM_GETSEL: which part of the EDIT's text is selected.
  public static int[] Selection(IntPtr root) {
    IntPtr e = EditHwnd(root);
    if (e == IntPtr.Zero) return null;
    IntPtr ps = Marshal.AllocHGlobal(4);
    IntPtr pe = Marshal.AllocHGlobal(4);
    Marshal.WriteInt32(ps, 0);
    Marshal.WriteInt32(pe, 0);
    SendMessage(e, 0x00B0 /* EM_GETSEL */, ps, pe);
    int a = Marshal.ReadInt32(ps);
    int b = Marshal.ReadInt32(pe);
    Marshal.FreeHGlobal(ps);
    Marshal.FreeHGlobal(pe);
    return new[]{a, b};
  }

  // Visible top-level windows of one class (used to spot a fresh Explorer).
  public static IntPtr[] ClassWindows(string cls) {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => {
      var sb = new StringBuilder(64);
      GetClassName(h, sb, 64);
      if (sb.ToString() == cls && IsWindowVisible(h)) list.Add(h);
      return true;
    }, IntPtr.Zero);
    return list.ToArray();
  }

  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern int GetWindowText(IntPtr h, StringBuilder s, int max);
  public static string Title(IntPtr h) {
    var sb = new StringBuilder(256);
    GetWindowText(h, sb, 256);
    return sb.ToString();
  }

  // Class of the window that actually owns the pixel (top of the z-order).
  public static string ClassAt(int x, int y) {
    POINT p = new POINT { X = x, Y = y };
    IntPtr h = WindowFromPoint(p);
    if (h == IntPtr.Zero) return "<none>";
    var sb = new StringBuilder(64);
    GetClassName(h, sb, 64);
    return sb.ToString();
  }
  [DllImport("user32.dll")] private static extern IntPtr WindowFromPoint(POINT p);
}
"@

[GtxCheck]::SetProcessDPIAware() | Out-Null

# Nothing below means anything with an old window around: the forwarding
# check counts windows, and a second process would share the profile.
$preWindows = [GtxCheck]::ClassWindows("GtxBrowserWindow")
if ($preWindows.Length -gt 0) {
    Write-Output "FAIL a GTX window is already open - close it before running ui-check"
    exit 1
}

function Scale([int]$v, [int]$dpi) { [int][Math]::Floor(($v * $dpi + 48) / 96) }

# Saves the drop-down panel's own surface (PrintWindow) for pixel scanning.
function SavePopup([string]$name) {
    $rc = [GtxCheck]::Popup($h)
    if ($null -eq $rc) { Write-Output ("NOTE no popup to save for {0}" -f $name); return }
    $pw = $rc[2] - $rc[0]
    $phh = $rc[3] - $rc[1]
    if ($pw -le 0 -or $phh -le 0) { return }
    $bmp = New-Object System.Drawing.Bitmap($pw, $phh)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    $ok = [GtxCheck]::PrintPopup($h, $hdc)
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    $bmp.Save([System.IO.Path]::GetFullPath(".\target\$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    if (-not $ok) { Write-Output ("NOTE PrintWindow said no for {0}" -f $name) }
}

# Every row carries an icon in the gutter left of its label. The strip is
# scanned while nothing is hovered, so anything there that is not the panel's
# own background has to be a glyph.
function CheckRowIcons([string]$png) {
    $path = [System.IO.Path]::GetFullPath(".\target\$png")
    if (-not (Test-Path $path)) { Write-Output ("NOTE no {0} to scan" -f $png); return }
    $bmp = [System.Drawing.Bitmap]::FromFile($path)
    # The panel background is by far the most common colour in its own image.
    $counts = @{}
    for ($y = 0; $y -lt $bmp.Height; $y += 2) {
        for ($x = 0; $x -lt $bmp.Width; $x += 2) {
            $c = $bmp.GetPixel($x, $y)
            $k = "{0},{1},{2}" -f ($c.R -shr 3), ($c.G -shr 3), ($c.B -shr 3)
            if ($counts.ContainsKey($k)) { $counts[$k] = $counts[$k] + 1 } else { $counts[$k] = 1 }
        }
    }
    $bgKey = "0,0,0"
    $bgN = -1
    foreach ($kv in $counts.GetEnumerator()) {
        if ($kv.Value -gt $bgN) { $bgN = $kv.Value; $bgKey = $kv.Key }
    }
    $parts = $bgKey -split ','
    $br = ([int]$parts[0]) * 8 + 4
    $bgc = ([int]$parts[1]) * 8 + 4
    $bb = ([int]$parts[2]) * 8 + 4
    $gx0 = Scale 10 $dpi
    $gx1 = Scale 28 $dpi
    $n = 0
    for ($y = (Scale 4 $dpi); $y -lt ($bmp.Height - (Scale 4 $dpi)); $y++) {
        for ($x = $gx0; $x -lt $gx1 -and $x -lt $bmp.Width; $x++) {
            $c = $bmp.GetPixel($x, $y)
            if ([Math]::Abs($c.R - $br) -gt 18 -or [Math]::Abs($c.G - $bgc) -gt 18 -or [Math]::Abs($c.B - $bb) -gt 18) { $n++ }
        }
    }
    $bmp.Dispose()
    if ($n -ge 8) {
        Write-Output ("PASS {0} rows carry icons in the gutter ({1} glyph pixels)" -f $png, $n)
    } else {
        Write-Output ("FAIL {0} has no icons in its gutter ({1} pixels)" -f $png, $n)
    }
}

# Two captures of the same panel, with and without the cursor over a row:
# hovering must repaint it.
function CheckHover([string]$beforePng, [string]$afterPng, [string]$label) {
    $p1 = [System.IO.Path]::GetFullPath(".\target\$beforePng")
    $p2 = [System.IO.Path]::GetFullPath(".\target\$afterPng")
    if (-not (Test-Path $p1) -or -not (Test-Path $p2)) {
        Write-Output ("NOTE no hover pair to compare for {0}" -f $label)
        return
    }
    $b1 = [System.Drawing.Bitmap]::FromFile($p1)
    $b2 = [System.Drawing.Bitmap]::FromFile($p2)
    if ($b1.Width -ne $b2.Width -or $b1.Height -ne $b2.Height) {
        Write-Output ("FAIL {0}: hover captures differ in size" -f $label)
        $b1.Dispose()
        $b2.Dispose()
        return
    }
    $n = 0
    for ($y = 0; $y -lt $b1.Height; $y += 2) {
        for ($x = 0; $x -lt $b1.Width; $x += 2) {
            $c1 = $b1.GetPixel($x, $y)
            $c2 = $b2.GetPixel($x, $y)
            if ([Math]::Abs($c1.R - $c2.R) -gt 8 -or [Math]::Abs($c1.G - $c2.G) -gt 8 -or [Math]::Abs($c1.B - $c2.B) -gt 8) { $n++ }
        }
    }
    $b1.Dispose()
    $b2.Dispose()
    if ($n -ge 60) {
        Write-Output ("PASS {0} highlight under the cursor ({1} pixels changed)" -f $label, $n)
    } else {
        Write-Output ("FAIL {0} hover does not repaint ({1} pixels changed)" -f $label, $n)
    }
}

$p = Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru
Start-Sleep -Milliseconds $WaitMs
$p.Refresh()
$h = $p.MainWindowHandle
if ($h -eq [IntPtr]::Zero) {
    Write-Output "FAIL no main window"
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    exit 1
}
Write-Output ("foreground: {0}" -f [GtxCheck]::Focus($h))
Start-Sleep -Milliseconds 800

$dpi = [int][GtxCheck]::GetDpiForWindow($h)
$c = [GtxCheck]::Client($h)
$cfgPath = Join-Path $env:LOCALAPPDATA "GTX Browser\config.toml"
$compact = $false
if (Test-Path $cfgPath) {
    $compact = ((Get-Content $cfgPath | Where-Object { $_ -match '^\s*compact_mode\s*=\s*true' }).Count -gt 0)
}
Write-Output ("dpi={0} client={1}x{2} compact={3}" -f $dpi, $c[2], $c[3], $compact)

$capW = if ($compact) { Scale 40 $dpi } else { Scale 46 $dpi }
$kebW = if ($compact) { Scale 30 $dpi } else { Scale 34 $dpi }
$rowH = if ($compact) { Scale 36 $dpi } else { Scale 44 $dpi }
$gap = Scale 4 $dpi
$dlW = $kebW

# --- 1+2: click the omnibox, type, expect the text in the EDIT and a
# visible popup. The click both focuses and proves the hit-test path.
$padW = Scale 12 $dpi
$btnW = if ($compact) { Scale 24 $dpi } else { Scale 28 $dpi }
$pillX = $padW + 3 * ($btnW + $gap) + (Scale 4 $dpi)
# Click the pill *left of* the EDIT (which starts 30 design units in): the
# chrome handles it as Hit::Omnibox and selects all; a click inside the EDIT
# would instead drop the caret where it lands.
[GtxCheck]::ClickAt($c[0] + $pillX + (Scale 8 $dpi), $c[1] + [int]($rowH / 2))
Start-Sleep -Milliseconds 500
$focus1 = [GtxCheck]::Focus($h)
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait("example")
Start-Sleep -Milliseconds 1200
$addr = [GtxCheck]::AddressText($h)
for ($try = 1; $try -lt 4 -and $addr -ne "example"; $try++) {
    # The first attempt can land while the page is still stealing focus.
    [GtxCheck]::Focus($h) | Out-Null
    [GtxCheck]::ClickAt($c[0] + $pillX + (Scale 8 $dpi), $c[1] + [int]($rowH / 2))
    Start-Sleep -Milliseconds 500
    [GtxCheck]::Focus($h) | Out-Null
    [System.Windows.Forms.SendKeys]::SendWait("^aexample")
    Start-Sleep -Milliseconds 1200
    $addr = [GtxCheck]::AddressText($h)
}

$addr = [GtxCheck]::AddressText($h)
$popup = [GtxCheck]::Popup($h)
Write-Output ("focus={0} address text: '{1}'" -f $focus1, $addr)
if ($addr -eq "example") { Write-Output "PASS typing lands in the address bar" } else { Write-Output "FAIL typing did not reach the address bar" }
if ($null -ne $popup) {
    Write-Output ("PASS suggestion panel visible at {0},{1} .. {2},{3} ({4}x{5})" -f `
        $popup[0], $popup[1], $popup[2], $popup[3], ($popup[2]-$popup[0]), ($popup[3]-$popup[1]))
    $cx = [int](($popup[0] + $popup[2]) / 2)
    $cy = [int](($popup[1] + $popup[3]) / 2)
    $own = [GtxCheck]::ClassAt($cx, $cy)
    Write-Output ("panel centre is covered by: {0}" -f $own)
} else {
    Write-Output "FAIL suggestion panel is not visible"
}

function Shot([string]$name, [int[]]$around = $null) {
    if ($null -ne $around) {
        $ox = [int](($around[0] + $around[2]) / 2)
        $oy = [int](($around[1] + $around[3]) / 2)
        Write-Output ("{0}: owner at capture time = {1}" -f $name, [GtxCheck]::ClassAt($ox, $oy))
    }
    Add-Type -AssemblyName System.Drawing
    Add-Type @"
using System;
using System.Runtime.InteropServices;
public class GtxShot {
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] private struct RECT { public int Left, Top, Right, Bottom; }
  public static int[] Window(IntPtr h) { RECT r; GetWindowRect(h, out r); return new[]{r.Left,r.Top,r.Right,r.Bottom}; }
}
"@
    $w4 = [GtxShot]::Window($h)
    $ww = $w4[2] - $w4[0]
    $wh = $w4[3] - $w4[1]
    $bmp = New-Object System.Drawing.Bitmap($ww, $wh)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($w4[0], $w4[1], 0, 0, (New-Object System.Drawing.Size($ww, $wh)))
    $g.Dispose()
    $outPath = [System.IO.Path]::GetFullPath(".\target\$name.png")
    $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Output "saved -> $outPath"
    if ($null -ne $around) {
        # Zoom the panel itself: screen rect -> bitmap coordinates.
        $mx = 30
        $x = $around[0] - $w4[0] - $mx
        $y = $around[1] - $w4[1] - $mx
        $cw = ($around[2] - $around[0]) + 2 * $mx
        $ch = ($around[3] - $around[1]) + 2 * $mx
        if ($x -lt 0) { $cw += $x; $x = 0 }
        if ($y -lt 0) { $ch += $y; $y = 0 }
        if ($x + $cw -gt $ww) { $cw = $ww - $x }
        if ($y + $ch -gt $wh) { $ch = $wh - $y }
        $rect = New-Object System.Drawing.Rectangle($x, $y, $cw, $ch)
        $crop = $bmp.Clone($rect, $bmp.PixelFormat)
        $zoom = New-Object System.Drawing.Bitmap(($cw * 2), ($ch * 2))
        $gz = [System.Drawing.Graphics]::FromImage($zoom)
        $gz.InterpolationMode = "NearestNeighbor"
        $gz.PixelOffsetMode = "Half"
        $gz.DrawImage($crop, 0, 0, ($cw * 2), ($ch * 2))
        $gz.Dispose()
        $crop.Dispose()
        $zPath = [System.IO.Path]::GetFullPath(".\target\$name-zoom.png")
        $zoom.Save($zPath, [System.Drawing.Imaging.ImageFormat]::Png)
        $zoom.Dispose()
        Write-Output "saved -> $zPath"
    }
    $bmp.Dispose()
}
Shot "ui-suggest" $popup

# The panel must be composited INTO the screen capture, not merely exist as
# a window: count pill-grey (#3a3a3a) pixels in the zoomed popup crop.
function CheckPanelPixels([string]$name) {
    $zp = [System.IO.Path]::GetFullPath(".\target\$name-zoom.png")
    if (-not (Test-Path $zp)) { Write-Output "NOTE no $name-zoom.png to scan"; return }
    $zb = [System.Drawing.Bitmap]::FromFile($zp)
    $tot = 0; $grey = 0
    for ($y = 0; $y -lt $zb.Height; $y += 4) {
        for ($x = 0; $x -lt $zb.Width; $x += 4) {
            $zc = $zb.GetPixel($x, $y); $tot++
            if ($zc.R -ge 50 -and $zc.R -le 70 -and
                [Math]::Abs($zc.R - $zc.G) -lt 6 -and [Math]::Abs($zc.G - $zc.B) -lt 6) { $grey++ }
        }
    }
    $zb.Dispose()
    $frac = $grey / [Math]::Max($tot, 1)
    if ($frac -ge 0.25) {
        Write-Output ("PASS {0} panel is rendered on screen ({1:P0} panel-grey pixels)" -f $name, $frac)
    } else {
        Write-Output ("FAIL {0} panel pixels missing from the screen capture ({1:P0} panel-grey)" -f $name, $frac)
    }
}
CheckPanelPixels "ui-suggest"
if ($null -ne $popup) {
    $pw = $popup[2] - $popup[0]
    $ph = $popup[3] - $popup[1]
    $pbmp = New-Object System.Drawing.Bitmap($pw, $ph)
    $pg = [System.Drawing.Graphics]::FromImage($pbmp)
    $phdc = $pg.GetHdc()
    $okPrint = [GtxCheck]::PrintPopup($h, $phdc)
    $pg.ReleaseHdc($phdc)
    $pg.Dispose()
    $pPath = [System.IO.Path]::GetFullPath(".\target\popup-own.png")
    $pbmp.Save($pPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $pc = $pbmp.GetPixel([int]($pw / 2), 20)
    $pbmp.Dispose()
    Write-Output ("popup PrintWindow ok={0}, saved, top-centre pixel = #{1}{2}{3}" -f $okPrint, $pc.R.ToString("x2"), $pc.G.ToString("x2"), $pc.B.ToString("x2"))
}

# --- row icons + hover on the suggestion panel (it shares the row
# renderer with the downloads list) -------------------------------------------
CheckRowIcons "popup-own.png"
$sugRect = [GtxCheck]::Popup($h)
if ($null -ne $sugRect) {
    $sugH = $sugRect[3] - $sugRect[1]
    $sugRows = [int][Math]::Round(($sugH - 2 * (Scale 4 $dpi) - 2) / (Scale 44 $dpi))
    if ($sugRows -ge 2) {
        # Row 0 is always pre-selected; row 1 is the one hover can change.
        $sx = [int](($sugRect[0] + $sugRect[2]) / 2)
        $sy = $sugRect[1] + (Scale 4 $dpi) + [int]((Scale 44 $dpi) * 1.5)
        [GtxCheck]::SetCursorPos($sx, $sy) | Out-Null
        Start-Sleep -Milliseconds 600
        SavePopup "popup-suggest-hover"
        CheckHover "popup-own.png" "popup-suggest-hover.png" "suggestion rows"
    } else {
        Write-Output "NOTE one-row suggestion list; hover is verified on the downloads panel"
    }
}

# --- 3: Esc closes it, then click the downloads button ----------------------
[System.Windows.Forms.SendKeys]::SendWait("{ESC}")
Start-Sleep -Milliseconds 500
if ($null -eq [GtxCheck]::Popup($h)) { Write-Output "PASS Esc closes the panel" } else { Write-Output "FAIL Esc left the panel open" }

$margin = 3 * $capW + $kebW + $gap + [int]($dlW / 2)
$px = $c[2] - $margin
$py = [int]($rowH / 2)
Write-Output ("downloads button at client {0},{1} (margin {2})" -f $px, $py, $margin)
[GtxCheck]::ClickAt($c[0] + $px, $c[1] + $py)

# Poll: the click may land while the page is busy in its nested pump.
$popup2 = $null
for ($i = 0; $i -lt 10 -and $null -eq $popup2; $i++) {
    Start-Sleep -Milliseconds 400
    $popup2 = [GtxCheck]::Popup($h)
}
if ($null -ne $popup2) {
    $w = $popup2[2] - $popup2[0]
    Write-Output ("PASS downloads panel visible at {0},{1} .. {2},{3} ({4}x{5})" -f `
        $popup2[0], $popup2[1], $popup2[2], $popup2[3], $w, ($popup2[3]-$popup2[1]))
    $cx2 = [int](($popup2[0] + $popup2[2]) / 2)
    $cy2 = [int](($popup2[1] + $popup2[3]) / 2)
    $own2 = [GtxCheck]::ClassAt($cx2, $cy2)
    Write-Output ("downloads panel centre is covered by: {0}" -f $own2)
} else {
    Write-Output "FAIL downloads panel is not visible"
}

# Screenshot the window for the record.
Shot "ui-downloads" $popup2
CheckPanelPixels "ui-downloads"

# --- 4: icons + hover on the downloads panel --------------------------------
SavePopup "popup-downloads-base"
CheckRowIcons "popup-downloads-base.png"
$dlRect = [GtxCheck]::Popup($h)
if ($null -ne $dlRect) {
    # Row 0 is the list (or the empty state); row 1 is always another row.
    $dx = [int](($dlRect[0] + $dlRect[2]) / 2)
    $dy = $dlRect[1] + (Scale 4 $dpi) + [int]((Scale 44 $dpi) * 1.5)
    [GtxCheck]::SetCursorPos($dx, $dy) | Out-Null
    Start-Sleep -Milliseconds 600
    SavePopup "popup-downloads-hover"
    CheckHover "popup-downloads-base.png" "popup-downloads-hover.png" "download rows"
}
# Close the panel again by toggling its button.
[GtxCheck]::ClickAt($c[0] + $px, $c[1] + $py)
Start-Sleep -Milliseconds 400

# --- 5: clicking the omnibox selects everything in it -----------------------
[GtxCheck]::ClickAt($c[0] + $pillX + (Scale 8 $dpi), $c[1] + [int]($rowH / 2))
Start-Sleep -Milliseconds 400
if ([GtxCheck]::AddressText($h).Length -eq 0) {
    # Nothing in the bar to select yet - focus is on it after the click.
    [System.Windows.Forms.SendKeys]::SendWait("selectme")
    Start-Sleep -Milliseconds 400
    [GtxCheck]::ClickAt($c[0] + $pillX + (Scale 8 $dpi), $c[1] + [int]($rowH / 2))
    Start-Sleep -Milliseconds 400
}
$selText = [GtxCheck]::AddressText($h)
$sel = [GtxCheck]::Selection($h)
$selShow = "?"
if ($null -ne $sel) { $selShow = "{0}..{1}" -f $sel[0], $sel[1] }
if ($null -ne $sel -and $selText.Length -gt 0 -and $sel[0] -eq 0 -and $sel[1] -eq $selText.Length) {
    Write-Output ("PASS clicking the omnibox selects all of '{0}' ({1})" -f $selText, $selShow)
} else {
    Write-Output ("FAIL click did not select all: selection={0} text='{1}'" -f $selShow, $selText)
}

# --- 6: a fresh tab hands over the address bar ------------------------------
[GtxCheck]::PostMessage($h, 0x111, [IntPtr]2004, [IntPtr]0) | Out-Null   # IDM_NEW_TAB
$ed = [GtxCheck]::EditHwnd($h)
$fc = [IntPtr]::Zero
for ($i = 0; $i -lt 20 -and $fc -ne $ed; $i++) {
    Start-Sleep -Milliseconds 300
    $fc = [GtxCheck]::FocusedChild($h)
}
if ($ed -ne [IntPtr]::Zero -and $fc -eq $ed) {
    Write-Output "PASS a fresh tab focuses the address bar"
} else {
    Write-Output ("FAIL a fresh tab did not focus the address bar (edit={0} focus={1})" -f $ed, $fc)
}

# --- 7: the loading bar runs along the omnibox ------------------------------
$accentHex = "#00e676"
if (Test-Path $cfgPath) {
    $accLine = Get-Content $cfgPath | Where-Object { $_ -match "accent\s*=" } | Select-Object -First 1
    if ($accLine -match "#([0-9a-fA-F]{6})") { $accentHex = "#" + $Matches[1] }
}
$ar = [Convert]::ToInt32($accentHex.Substring(1, 2), 16)
$ag = [Convert]::ToInt32($accentHex.Substring(3, 2), 16)
$ab = [Convert]::ToInt32($accentHex.Substring(5, 2), 16)
[GtxCheck]::Focus($h) | Out-Null
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait("https://en.wikipedia.org")
Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait("{ENTER}")
# The left end of the omnibox: the bar lives there, and nothing else in the
# row wears the accent colour (the active tab's underline sits further right,
# past the pill).
$bx = $c[0] + $pillX + (Scale 8 $dpi)
$bw = Scale 52 $dpi
$bh = $rowH
$seen = -1
for ($i = 0; $i -lt 60 -and $seen -lt 0; $i++) {
    Start-Sleep -Milliseconds 150
    $bmp = New-Object System.Drawing.Bitmap($bw, $bh)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($bx, $c[1], 0, 0, (New-Object System.Drawing.Size($bw, $bh)))
    $g.Dispose()
    $n = 0
    for ($y = 0; $y -lt $bh; $y++) {
        for ($x = 0; $x -lt $bw; $x++) {
            $pc = $bmp.GetPixel($x, $y)
            if ([Math]::Abs($pc.R - $ar) -lt 45 -and [Math]::Abs($pc.G - $ag) -lt 45 -and [Math]::Abs($pc.B - $ab) -lt 45) { $n++ }
        }
    }
    $bmp.Dispose()
    if ($n -ge 3) { $seen = $i }
}
if ($seen -ge 0) {
    Write-Output ("PASS loading bar appeared on the omnibox (sample {0})" -f $seen)
} else {
    Write-Output "FAIL loading bar never appeared on the omnibox"
}

# --- 8: "open the downloads folder" really opens it -------------------------
$expBefore = [GtxCheck]::ClassWindows("CabinetWClass")
[GtxCheck]::PostMessage($h, 0x111, [IntPtr]2042, [IntPtr]0) | Out-Null   # IDM_OPEN_DOWNLOADS
$found = [IntPtr]::Zero
for ($i = 0; $i -lt 25 -and $found -eq [IntPtr]::Zero; $i++) {
    Start-Sleep -Milliseconds 400
    $now = [GtxCheck]::ClassWindows("CabinetWClass")
    foreach ($w in $now) {
        $isOld = $false
        foreach ($b in $expBefore) { if ($b -eq $w) { $isOld = $true } }
        if (-not $isOld) { $found = $w }
    }
}
if ($found -ne [IntPtr]::Zero) {
    Write-Output "PASS open-downloads-folder shows the folder in Explorer"
    Start-Sleep -Milliseconds 600
    [GtxCheck]::PostMessage($found, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null   # WM_CLOSE
} else {
    Write-Output "FAIL open-downloads-folder did not show the folder"
}

# --- 9: the settings page opens (its General section carries the accent
# "Set as default" button, and only that section shows at first) -------------
[GtxCheck]::PostMessage($h, 0x111, [IntPtr]2014, [IntPtr]0) | Out-Null   # IDM_OPEN_SETTINGS
$setW = $c[2]
$setCap = Scale 360 $dpi
$setH = [Math]::Min($setCap, $c[3] - $rowH - 4)
$setY = $c[1] + $rowH + 2
$setSeen = -1
$title = ""
for ($i = 0; $i -lt 25 -and $setSeen -lt 0; $i++) {
    Start-Sleep -Milliseconds 400
    $title = [GtxCheck]::Title($h)
    $bmp = New-Object System.Drawing.Bitmap($setW, $setH)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($c[0], $setY, 0, 0, (New-Object System.Drawing.Size($setW, $setH)))
    $g.Dispose()
    $n = 0
    for ($y = 0; $y -lt $setH; $y += 2) {
        for ($x = 0; $x -lt $setW; $x += 2) {
            $pc = $bmp.GetPixel($x, $y)
            # The accent, as the web view actually paints it (the engine's
            # colour pipeline shifts it a little, so match the hue, not the hex).
            if ($pc.G -gt 150 -and $pc.G -gt ($pc.R + 60) -and $pc.G -gt ($pc.B + 40)) { $n++ }
        }
    }
    $bmp.Dispose()
    if ($n -ge 15) { $setSeen = $n }
}
if ($title -match "Settings" -and $setSeen -ge 0) {
    Write-Output ("PASS settings page opens on General with its accent button ({0} accent pixels)" -f $setSeen)
} else {
    Write-Output ("FAIL settings page did not render (title='{0}' accent={1})" -f $title, $setSeen)
}


# --- 10: hydration - Rust hands the config over, so the footer leaves
# "Loading..." and reports "All changes saved". ----------------------------
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
function UiaByName([string]$text) {
    $pc = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $text)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($h)
    return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $pc)
}
$hydrated = $false
for ($i = 0; $i -lt 25 -and -not $hydrated; $i++) {
    Start-Sleep -Milliseconds 400
    $hydrated = $null -ne (UiaByName "All changes saved")
}
if ($hydrated) {
    Write-Output "PASS settings hydrate: the footer reports 'All changes saved'"
} else {
    Write-Output "FAIL settings never hydrated - the footer is still on 'Loading...'"
}

# --- 11: the round trip - flipping a switch on the page rewrites
# config.toml, and flipping it back puts the value back exactly. ----------
$homeKey = "show_home_button"
function CfgVal([string]$key) {
    $hit = Get-Content $cfgPath | Where-Object { $_ -match ("^\s*" + $key + "\s*=") } | Select-Object -First 1
    return "$hit"
}
if (-not (Test-Path $cfgPath)) {
    Write-Output "NOTE no config.toml - settings round trip not checked"
} else {
    $home0 = CfgVal $homeKey
    $navBtn = $null
    for ($i = 0; $i -lt 12 -and $null -eq $navBtn; $i++) {
        Start-Sleep -Milliseconds 400
        $navBtn = UiaByName "Appearance"
    }
    $navOk = $false
    if ($null -ne $navBtn) {
        try {
            $ip = $navBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
            $ip.Invoke()
            $navOk = $true
        } catch {
            Write-Output ("NOTE appearance chip invoke failed: {0}" -f $_.Exception.Message)
        }
    }
    Start-Sleep -Milliseconds 700
    $boxC = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::CheckBox)
    $boxes = [System.Windows.Automation.AutomationElement]::FromHandle($h).FindAll([System.Windows.Automation.TreeScope]::Descendants, $boxC)
    Write-Output ("settings exposes {0} switch(es) after the section change" -f $boxes.Count)
    $flipOk = $false
    $backOk = $false
    if ($navOk -and $home0 -and $boxes.Count -ge 2) {
        $tp = $boxes.Item(1).GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
        $tp.Toggle()
        for ($i = 0; $i -lt 15 -and -not $flipOk; $i++) {
            Start-Sleep -Milliseconds 400
            if ((CfgVal $homeKey) -ne $home0) { $flipOk = $true }
        }
        if ($flipOk) {
            $boxes = [System.Windows.Automation.AutomationElement]::FromHandle($h).FindAll([System.Windows.Automation.TreeScope]::Descendants, $boxC)
            $tp = $boxes.Item(1).GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
            $tp.Toggle()
            for ($i = 0; $i -lt 15 -and -not $backOk; $i++) {
                Start-Sleep -Milliseconds 400
                if ((CfgVal $homeKey) -eq $home0) { $backOk = $true }
            }
        }
    }
    if ($flipOk -and $backOk) {
        Write-Output "PASS a settings edit reaches config.toml and the flip-back restores it"
    } else {
        Write-Output ("FAIL settings round trip (nav={0} flipped={1} restored={2})" -f $navOk, $flipOk, $backOk)
    }
}


# --- 12: a second launch hands its link to the open window and exits - no
# duplicate window, no dropped link. ---------------------------------------
$child = Start-Process -FilePath $Exe -ArgumentList "https://example.org" -PassThru
$childGone = $child.WaitForExit(8000)
$linkSeen = $false
for ($i = 0; $i -lt 30 -and -not $linkSeen; $i++) {
    Start-Sleep -Milliseconds 500
    $t = [GtxCheck]::Title($h)
    $a = [GtxCheck]::AddressText($h)
    if ($t -match "Example" -or $a -match "example\.org") { $linkSeen = $true }
}
if (-not $childGone) {
    Write-Output "NOTE the second process did not exit - killing it"
    try { $child.Kill() } catch { }
}
$winCount = ([GtxCheck]::ClassWindows("GtxBrowserWindow")).Length
if ($linkSeen -and $childGone -and $winCount -eq 1) {
    Write-Output "PASS a second launch forwards its link and leaves one window"
} else {
    Write-Output ("FAIL link forwarding (seen={0} child-exited={1} windows={2})" -f $linkSeen, $childGone, $winCount)
}


# --- 13: the history page shows the sites' own favicons -------------------
$histPath = Join-Path $env:LOCALAPPDATA "GTX Browser\history.toml"
$histWeb = $false
if (Test-Path $histPath) {
    $histWeb = ((Get-Content $histPath | Where-Object { $_ -match 'url\s*=\s*"https?://' }).Count -gt 0)
}
[GtxCheck]::PostMessage($h, 0x111, [IntPtr]2041, [IntPtr]0) | Out-Null   # IDM_HISTORY
$histShown = $false
for ($i = 0; $i -lt 25 -and -not $histShown; $i++) {
    Start-Sleep -Milliseconds 400
    if ([GtxCheck]::Title($h) -match "History") { $histShown = $true }
}
if (-not $histShown) {
    Write-Output "FAIL the history page never opened"
} elseif (-not $histWeb) {
    Write-Output "NOTE no web history rows - nothing to show favicons for"
} else {
    Start-Sleep -Milliseconds 900
    Shot "ui-history"
    # The icon gutter: a 16px column starting 40css from the list's left
    # edge; anything painted there that is not the page background is an
    # icon (a real one or its letter-tile fallback).
    $winR = [GtxShot]::Window($h)
    $hp = [System.IO.Path]::GetFullPath(".\target\ui-history.png")
    if (Test-Path $hp) {
        $hbm = [System.Drawing.Bitmap]::FromFile($hp)
        $ox = ($c[0] - $winR[0]) + (Scale 34 $dpi)
        $oxe = ($c[0] - $winR[0]) + (Scale 74 $dpi)
        $oy = ($c[1] - $winR[1]) + $rowH + (Scale 85 $dpi)
        $oye = ($c[1] - $winR[1]) + $rowH + (Scale 500 $dpi)
        if ($oxe -gt $hbm.Width) { $oxe = $hbm.Width }
        if ($oye -gt $hbm.Height) { $oye = $hbm.Height }
        if ($oy -lt 0) { $oy = 0 }
        if ($ox -lt 0) { $ox = 0 }
        $counts = @{}
        for ($y = $oy; $y -lt $oye; $y += 2) {
            for ($x = $ox; $x -lt $oxe; $x += 2) {
                $qc = $hbm.GetPixel($x, $y)
                $qk = "{0},{1},{2}" -f ($qc.R -shr 3), ($qc.G -shr 3), ($qc.B -shr 3)
                if ($counts.ContainsKey($qk)) { $counts[$qk] = $counts[$qk] + 1 } else { $counts[$qk] = 1 }
            }
        }
        $bgKey = "0,0,0"
        $bgN = -1
        foreach ($kv in $counts.GetEnumerator()) { if ($kv.Value -gt $bgN) { $bgN = $kv.Value; $bgKey = $kv.Key } }
        $bp = $bgKey -split ','
        $br2 = ([int]$bp[0]) * 8 + 4
        $bg2 = ([int]$bp[1]) * 8 + 4
        $bb2 = ([int]$bp[2]) * 8 + 4
        $gn = 0
        for ($y = $oy; $y -lt $oye; $y++) {
            for ($x = $ox; $x -lt $oxe; $x++) {
                $qc = $hbm.GetPixel($x, $y)
                if ([Math]::Abs($qc.R - $br2) -gt 18 -or [Math]::Abs($qc.G - $bg2) -gt 18 -or [Math]::Abs($qc.B - $bb2) -gt 18) { $gn++ }
            }
        }
        $sat = 0
        for ($y = $oy; $y -lt $oye; $y++) {
            for ($x = $ox; $x -lt $oxe; $x++) {
                $qc = $hbm.GetPixel($x, $y)
                $mxv = [Math]::Max($qc.R, [Math]::Max($qc.G, $qc.B))
                $mnv = [Math]::Min($qc.R, [Math]::Min($qc.G, $qc.B))
                if (($mxv - $mnv) -ge 60 -and $mxv -ge 120) { $sat++ }
            }
        }
        if ($sat -ge 40) {
            Write-Output ("PASS the history page paints real favicons ({0} colourful icon pixels)" -f $sat)
        } else {
            Write-Output ("FAIL the history page shows letter tiles only ({0} colourful pixels)" -f $sat)
        }
        $hbm.Dispose()
        if ($gn -ge 60) {
            Write-Output ("PASS the history list paints its icon gutter ({0} glyph pixels)" -f $gn)
        } else {
            Write-Output ("FAIL the history icon gutter is empty ({0} pixels)" -f $gn)
        }
    }
}

# Did a page hand us a favicon?
$logPath = Join-Path $env:LOCALAPPDATA "GTX Browser\gtx.log"
if (Test-Path $logPath) {
    $fav = Get-Content $logPath -Tail 300 | Where-Object { $_ -match "favicon" }
    if ($fav) { Write-Output ("PASS {0}" -f $fav[-1]) } else { Write-Output "NOTE no favicon recorded in the log" }
}

# A second launch that failed to forward leaves a stray window behind;
# do not make the next run fail on it.
foreach ($w in [GtxCheck]::ClassWindows("GtxBrowserWindow")) {
    if ($w -ne $h) {
        $wpid = 0
        [GtxCheck]::GetWindowThreadProcessId($w, [ref]$wpid) | Out-Null
        if ($wpid -gt 0) { Stop-Process -Id $wpid -Force -ErrorAction SilentlyContinue }
    }
}
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
