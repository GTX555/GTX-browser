# GTX Browser installer round-trip: install -> verify -> smoke -> uninstall
# -> verify everything is gone. Run from the repo root:
#
#   powershell -File tools/roundtrip.ps1
#
# Everything is checked against real side effects (files, shortcuts, registry)
# rather than trusting the installer's own report. ASCII-only file (see the
# note in package.ps1).
param(
    [string]$Stage = ".\dist\GTX-Browser"
)

$ErrorActionPreference = "Stop"
$setup = Join-Path $Stage "gtx-setup.exe"
if (-not (Test-Path $setup)) { Write-Output "missing $setup (run package.ps1 first)"; exit 1 }

$dir = Join-Path $env:LOCALAPPDATA "Programs\GTX Browser"
$desktop = [Environment]::GetFolderPath("Desktop")
$programs = [Environment]::GetFolderPath("Programs")
$desktopLnk = Join-Path $desktop "GTX Browser.lnk"
$programsLnk = Join-Path $programs "GTX Browser.lnk"
$arp = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\GTX Browser"

function Check([string]$label, [bool]$ok) {
    Write-Output ("  {0} {1}" -f $(if ($ok) { "[ok]" } else { "[!!]" }), $label)
    return $ok
}

# Application Control can block a freshly built exe (4551) -- retry.
function Run-Retry([string]$file, [string[]]$ArgList) {
    for ($i = 1; $i -le 5; $i++) {
        $p = Start-Process -FilePath $file -ArgumentList $ArgList -Wait -PassThru -WindowStyle Hidden
        if ($p.ExitCode -eq 0) { return 0 }
        Write-Output "    attempt $i on $file -> exit $($p.ExitCode), retrying in 12s"
        Start-Sleep -Seconds 12
    }
    return $p.ExitCode
}

Write-Output "== install =="
$rc = Run-Retry $setup @("--quiet")
if ($rc -ne 0) { Write-Output "install failed (exit $rc)"; exit 1 }
Start-Sleep -Seconds 2

Write-Output "== verify installed =="
$fail = $false
$fail = -not (Check "browser exe copied" (Test-Path (Join-Path $dir "gtx-browser.exe"))) -or $fail
$fail = -not (Check "setup exe copied (for uninstall)" (Test-Path (Join-Path $dir "gtx-setup.exe"))) -or $fail
$fail = -not (Check "Desktop shortcut" (Test-Path $desktopLnk)) -or $fail
$fail = -not (Check "Start Menu shortcut" (Test-Path $programsLnk)) -or $fail
$fail = -not (Check "Add/Remove Programs entry" (Test-Path $arp)) -or $fail
if ($fail) { Write-Output "install verification failed"; exit 1 }

Write-Output "== smoke the installed browser =="
$browser = Join-Path $dir "gtx-browser.exe"
$rc = Run-Retry $browser @("--smoke", "3500")
if ($rc -ne 0) { Write-Output "installed browser smoke failed (exit $rc)"; exit 1 }
$fail = $false
$fail = -not (Check "smoke exit 0" ($rc -eq 0)) -or $fail

Write-Output "== uninstall =="
$rc = Run-Retry (Join-Path $dir "gtx-setup.exe") @("--uninstall", "--quiet")
if ($rc -ne 0) { Write-Output "uninstall failed (exit $rc)"; exit 1 }
# The folder is deleted by a deferred helper after setup exits; give it time.
Start-Sleep -Seconds 10

Write-Output "== verify removed =="
$fail = $false
$fail = -not (Check "Desktop shortcut gone" (-not (Test-Path $desktopLnk))) -or $fail
$fail = -not (Check "Start Menu shortcut gone" (-not (Test-Path $programsLnk))) -or $fail
$fail = -not (Check "Add/Remove entry gone" (-not (Test-Path $arp))) -or $fail
$fail = -not (Check "install folder gone" (-not (Test-Path $dir))) -or $fail

Write-Output ""
if ($fail) { Write-Output "ROUND-TRIP: FAIL"; exit 1 }
Write-Output "ROUND-TRIP: PASS"
Write-Output "(user settings kept: $env:LOCALAPPDATA\GTX Browser)"
