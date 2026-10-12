# Dump the exact codepoints of GTX control names (checks for text mangling).
param([int]$TimeoutSec = 20)

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$proc = $null
while ((Get-Date) -lt $deadline) {
    $proc = Get-Process gtx-browser -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($proc) { break }
    Start-Sleep -Milliseconds 200
}
if (-not $proc) { Write-Output "gtx-browser is not running"; exit 1 }
Start-Sleep -Seconds 2

Add-Type -AssemblyName UIAutomationClient
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $proc.Id)
$win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
if (-not $win) { Write-Output "main window not found"; exit 1 }

$all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.Condition]::TrueCondition)
foreach ($e in $all) {
    $n = $e.Current.Name
    if ($n -and $n.Length -gt 0 -and $n -notmatch 'duckduckgo|DuckDuckGo|hphp') {
        $codes = ($n.ToCharArray() | ForEach-Object { "U+{0:X4}" -f [int]$_ }) -join ' '
        Write-Output ("len={0} codepoints: {1}" -f $n.Length, $codes)
    }
}
