# Inspect a running GTX Browser instance:
#   - process footprint (GTX + its own WebView2 children)
#   - the UI Automation tree (window title, buttons, address bar value)
#
# Usage:  powershell -NoProfile -ExecutionPolicy Bypass -File tools\inspect.ps1
param([int]$TimeoutSec = 20)

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$proc = $null
while ((Get-Date) -lt $deadline) {
    $proc = Get-Process gtx-browser -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($proc) { break }
    Start-Sleep -Milliseconds 200
}
if (-not $proc) {
    Write-Output "gtx-browser is not running"
    exit 1
}

# Give the homepage a moment to load so title/URL are populated.
Start-Sleep -Seconds 3

Write-Output "== processes"
$total = 0
$gtx = @(Get-Process gtx-browser -ErrorAction SilentlyContinue)
foreach ($p in $gtx) {
    $mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
    $total += $mb
    Write-Output ("{0,-16} {1,6} {2,8} MB" -f $p.ProcessName, $p.Id, $mb)
}
$children = Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -like '*GTX Browser*' }
foreach ($c in @($children)) {
    $p = Get-Process -Id $c.ProcessId -ErrorAction SilentlyContinue
    if ($p) {
        $mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
        $total += $mb
        Write-Output ("{0,-16} {1,6} {2,8} MB" -f $p.ProcessName, $p.Id, $mb)
    }
}
Write-Output ("TOTAL: {0} MB across {1} process(es)" -f [math]::Round($total, 1), (@($gtx).Count + @($children).Count))

Write-Output "== ui automation"
Add-Type -AssemblyName UIAutomationClient
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $proc.Id)
$win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
if (-not $win) {
    Write-Output "main window not found"
    exit 1
}
Write-Output ("window title: '{0}'" -f $win.Current.Name)

$trueCond = [System.Windows.Automation.Condition]::TrueCondition
$all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $trueCond)
foreach ($e in $all) {
    $ct = $e.Current.ControlType.ProgrammaticName -replace 'ControlType\.', ''
    $name = $e.Current.Name
    $hwnd = $e.Current.NativeWindowHandle
    $r = $e.Current.BoundingRectangle
    $rect = "{0},{1} {2}x{3}" -f [math]::Round($r.X), [math]::Round($r.Y), [math]::Round($r.Width), [math]::Round($r.Height)
    if ($ct -eq 'Edit') {
        $value = ''
        try {
            $vp = $e.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
            $value = $vp.Current.Value
        } catch {}
        Write-Output ("{0,-8} hwnd={1,-10} {2,-16} '{3}' value='{4}'" -f $ct, $hwnd, $rect, $name, $value)
    } else {
        Write-Output ("{0,-8} hwnd={1,-10} {2,-16} '{3}'" -f $ct, $hwnd, $rect, $name)
    }
}
