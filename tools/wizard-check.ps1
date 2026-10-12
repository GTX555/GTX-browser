# Drives gtx-setup.exe through the whole wizard and checks each page really
# appears: welcome, licence (with Next gated until the box is ticked), options,
# progress and finish. Captures a screenshot per page into target/.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/wizard-check.ps1
#
# ASCII only, PowerShell 5.1 compatible.

param(
    [string]$Exe = ".\target\debug\gtx-setup.exe",
    [string]$OutDir = ".\target",
    [switch]$SkipInstall
) 

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public class GtxWiz {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", EntryPoint="SendMessageW", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessageTextW(IntPtr h, int m, IntPtr w, StringBuilder l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetDlgCtrlID(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h, int id);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, int m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, int m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint cmd);
  public delegate bool EnumProc(IntPtr h, IntPtr p);
  const uint GW_CHILD = 5, GW_HWNDNEXT = 2;

  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

  public static IntPtr Find(uint pid, string cls) {
    IntPtr found = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr p) {
      uint wpid; GetWindowThreadProcessId(h, out wpid);
      if (wpid != pid) return true;
      StringBuilder sb = new StringBuilder(128);
      GetClassNameW(h, sb, sb.Capacity);
      if (sb.ToString() == cls) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }

  // Walking GW_CHILD/GW_HWNDNEXT is more dependable than EnumChildWindows
  // when the enumerator has to hand values back to PowerShell.
  public static string ChildClasses(IntPtr parent) {
    StringBuilder sb = new StringBuilder();
    IntPtr c = GetWindow(parent, GW_CHILD);
    while (c != IntPtr.Zero) {
      if (sb.Length > 0) sb.Append(",");
      sb.Append(Class(c));
      c = GetWindow(c, GW_HWNDNEXT);
    }
    return sb.ToString();
  }
  public static IntPtr FirstChildOfClass(IntPtr parent, string cls) {
    IntPtr c = GetWindow(parent, GW_CHILD);
    while (c != IntPtr.Zero) {
      if (string.Equals(Class(c), cls, StringComparison.OrdinalIgnoreCase)) return c;
      c = GetWindow(c, GW_HWNDNEXT);
    }
    return IntPtr.Zero;
  }

  public static string Class(IntPtr h) { StringBuilder s = new StringBuilder(128); GetClassNameW(h, s, s.Capacity); return s.ToString(); }
  // GetWindowText cannot read another process' controls; WM_GETTEXT can.
  public static string Text(IntPtr h) { StringBuilder s = new StringBuilder(8192); SendMessageTextW(h, 0x000D, (IntPtr)8192, s); return s.ToString(); }

  public static string[] StaticTexts(IntPtr parent) {
    List<string> texts = new List<string>();
    IntPtr c = GetWindow(parent, GW_CHILD);
    while (c != IntPtr.Zero) {
      if (Class(c) == "Static") texts.Add(Text(c));
      c = GetWindow(c, GW_HWNDNEXT);
    }
    return texts.ToArray();
  }
  public static int[] Rect(IntPtr h) { RECT r; GetWindowRect(h, out r); return new[]{r.Left, r.Top, r.Right-r.Left, r.Bottom-r.Top}; }
}
"@

[GtxWiz]::SetProcessDPIAware() | Out-Null

$script:fail = $false
function Check($name, $ok, $detail) {
    if ($ok) { Write-Output "PASS $name $detail" }
    else { Write-Output "FAIL $name $detail"; $script:fail = $true }
}

# Control ids, matching src/bin/gtx-setup.rs.
$ID_ACCEPT = 1001; $ID_DESKTOP = 1002; $ID_STARTMENU = 1003; $ID_LAUNCH = 1004
$ID_BACK = 1010; $ID_NEXT = 1011; $ID_CANCEL = 1012
$WM_COMMAND = 0x0111; $BM_GETCHECK = 0x00F0; $BM_SETCHECK = 0x00F1

function Click([IntPtr]$h, [int]$id) {
    [GtxWiz]::PostMessageW($h, $WM_COMMAND, [IntPtr]$id, [IntPtr]::Zero) | Out-Null
    Start-Sleep -Milliseconds 400
}
# Same, but returns at once so a fast page change can still be observed.
function ClickNow([IntPtr]$h, [int]$id) {
    [GtxWiz]::PostMessageW($h, $WM_COMMAND, [IntPtr]$id, [IntPtr]::Zero) | Out-Null
}
function IsChecked([IntPtr]$c) {
    [GtxWiz]::SendMessageW($c, $BM_GETCHECK, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32() -eq 1
}
# Screenshot straight off the screen, so the check proves what a user sees.
function Shot([IntPtr]$h, [string]$path) {
    $r = [GtxWiz]::Rect($h) 
    $bmp = New-Object System.Drawing.Bitmap($r[2], $r[3]) 
    $g = [System.Drawing.Graphics]::FromImage($bmp) 
    $g.CopyFromScreen($r[0], $r[1], 0, 0, (New-Object System.Drawing.Size($r[2], $r[3]))) 
    $g.Dispose() 
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png) 
    $bmp.Dispose() 
    return "$($r[2])x$($r[3])"
}

if (-not (Test-Path $Exe)) { Write-Output "FAIL missing $Exe"; exit 1 }

$p = Start-Process -FilePath $Exe -PassThru
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40 -and $hwnd -eq [IntPtr]::Zero; $i++) {
    Start-Sleep -Milliseconds 150
    $hwnd = [GtxWiz]::Find([uint32]$p.Id, "GtxSetupWizard") 
}
Check "wizard_window_opens" ($hwnd -ne [IntPtr]::Zero) ("pid=" + $p.Id) 
if ($hwnd -eq [IntPtr]::Zero) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue; exit 1 }

$title = [GtxWiz]::Text($hwnd) 
Check "window_title" ($title -eq "GTX Browser Setup") ("'" + $title + "'") 

# --- page 1: welcome -------------------------------------------------------
$size = Shot $hwnd "$OutDir\wiz-1-welcome.png"
Check "welcome_screenshot" (Test-Path "$OutDir\wiz-1-welcome.png") $size

$next = [GtxWiz]::GetDlgItem($hwnd, $ID_NEXT) 
Check "next_enabled_on_welcome" ([GtxWiz]::IsWindowEnabled($next)) ""
Check "back_hidden_on_welcome" (-not [GtxWiz]::IsWindowVisible([GtxWiz]::GetDlgItem($hwnd, $ID_BACK))) ""

$classList = [GtxWiz]::ChildClasses($hwnd)
Check "controls_created" (($classList -match "(?i)edit") -and ($classList -match "(?i)progress") -and ($classList -match "(?i)button")) $classList 

# --- page 2: licence -------------------------------------------------------
Click $hwnd $ID_NEXT
[void](Shot $hwnd "$OutDir\wiz-2-license.png") 
$nextOn = [GtxWiz]::GetDlgItem($hwnd, $ID_NEXT) 
Check "next_blocked_until_terms_accepted" (-not [GtxWiz]::IsWindowEnabled($nextOn)) ""

$termsCtl = [GtxWiz]::FirstChildOfClass($hwnd, "EDIT")
$terms = [GtxWiz]::Text($termsCtl) 
Check "terms_are_shown" ($terms -match "Terms of Use" -and $terms.Length -gt 800) ("$($terms.Length) chars") 
Check "terms_visible" ([GtxWiz]::IsWindowVisible($termsCtl)) ""
Check "back_visible_on_license" ([GtxWiz]::IsWindowVisible([GtxWiz]::GetDlgItem($hwnd, $ID_BACK))) ""

$accept = [GtxWiz]::GetDlgItem($hwnd, $ID_ACCEPT) 
[GtxWiz]::SendMessageW($accept, $BM_SETCHECK, [IntPtr]1, [IntPtr]::Zero) | Out-Null
Click $hwnd $ID_ACCEPT
Check "next_unblocked_after_accepting" ([GtxWiz]::IsWindowEnabled($nextOn)) ""

# --- page 3: options -------------------------------------------------------
Click $hwnd $ID_NEXT
[void](Shot $hwnd "$OutDir\wiz-3-options.png") 
$desk = [GtxWiz]::GetDlgItem($hwnd, $ID_DESKTOP) 
$start = [GtxWiz]::GetDlgItem($hwnd, $ID_STARTMENU) 
Check "options_page_shows_shortcut_boxes" ([GtxWiz]::IsWindowVisible($desk) -and [GtxWiz]::IsWindowVisible($start)) ""
Check "shortcut_boxes_default_on" ((IsChecked $desk) -and (IsChecked $start)) ""
Check "next_says_install" ([GtxWiz]::Text($nextOn) -eq "Install") ("'" + [GtxWiz]::Text($nextOn) + "'") 

if ($SkipInstall) {
    Click $hwnd $ID_CANCEL
    Start-Sleep -Milliseconds 600} else {
    # --- page 4/5: progress then finish ------------------------------------
    # The install is fast, so watch for the progress page instead of sleeping
    # past it: the step label must show one of STEP_LABELS at least once.
    $steps = @("Checking the files", "Closing any running", "Creating the install folder",
               "Copying GTX Browser", "Creating shortcuts", "Registering the Apps", "Verifying the installation")
    ClickNow $hwnd $ID_NEXT
    $sawStep = $false
    $sawBusyUi = $false
    for ($i = 0; $i -lt 400; $i++) {
        foreach ($s in [GtxWiz]::StaticTexts($hwnd)) {
            if ($steps | Where-Object { $s -like "$_*" }) {
                if (-not $sawStep) { [void](Shot $hwnd "$OutDir\wiz-4-progress.png") }
                $sawStep = $true
            }
        }
        if (-not [GtxWiz]::IsWindowEnabled($nextOn)) { $sawBusyUi = $true }
        if ([GtxWiz]::Text($nextOn) -eq "Finish") { break }
        Start-Sleep -Milliseconds 10
    }
    Check "progress_page_shows_step_labels" $sawStep ""
    Check "progress_page_locks_next" $sawBusyUi ""
    [void](Shot $hwnd "$OutDir\wiz-5-finish.png")
    $nextLabel = [GtxWiz]::Text($nextOn)
    Check "install_reaches_the_finish_page" ($nextLabel -eq "Finish") ("'" + $nextLabel + "'")
    Check "finish_has_no_back_button" (-not [GtxWiz]::IsWindowVisible([GtxWiz]::GetDlgItem($hwnd, $ID_BACK))) ""$nextLabel = [GtxWiz]::Text($nextOn)
    $launch = [GtxWiz]::GetDlgItem($hwnd, $ID_LAUNCH) 
    Check "finish_offers_launch" ([GtxWiz]::IsWindowVisible($launch) -and (IsChecked $launch)) ""
    # Untick it so closing the wizard does not start the browser.
    [GtxWiz]::SendMessageW($launch, $BM_SETCHECK, [IntPtr]0, [IntPtr]::Zero) | Out-Null
    Click $hwnd $ID_LAUNCH
    Click $hwnd $ID_NEXT
    for ($i = 0; $i -lt 40 -and -not $p.HasExited; $i++) { Start-Sleep -Milliseconds 150; $p.Refresh() }
    Check "wizard_closes_on_finish" ($p.HasExited) ""
    if ($p.HasExited) { Check "exit_code" ($p.ExitCode -eq 0) ("rc=" + $p.ExitCode) }
}

if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
if ($script:fail) { Write-Output "WIZARD CHECK: FAIL"; exit 1 }
Write-Output "WIZARD CHECK: PASS"
exit 0