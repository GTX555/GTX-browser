# GTX Browser packager: release build -> dist/GTX-Browser/ -> zip.
#
#   powershell -File tools/package.ps1            # build + stage + zip
#   powershell -File tools/package.ps1 -NoBuild   # stage + zip only
#
# Output: dist/GTX-Browser/          (portable folder: both exes)
#         dist/GTX-Browser-<ver>.zip (portable archive)
#
# NOTE: keep this file ASCII-only. Windows PowerShell reads BOM-less files as
# the ANSI codepage, and a UTF-8 em-dash there becomes a smart quote, which
# PowerShell accepts as a string delimiter and derails parsing.
param(
    [switch]$NoBuild,
    [string]$Dist = ".\dist"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot   # repo root (gtx-browser/)
Set-Location $root

$ver = (Select-String -Path .\Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
Write-Output "GTX Browser $ver"

if (-not $NoBuild) {
    # Application Control can deny a freshly written build artifact (error
    # 4551) -- retry a few times before giving up.
    $ok = $false
    for ($i = 1; $i -le 5; $i++) {
        # No 2>&1 here: in Windows PowerShell that converts cargo's stderr
        # into ErrorRecords, which $ErrorActionPreference = "Stop" turns fatal.
        & cargo build --release
        if ($LASTEXITCODE -eq 0) { $ok = $true; break }
        Write-Output "build attempt $i failed, retrying in 12s..."
        Start-Sleep -Seconds 12
    }
    if (-not $ok) { Write-Output "release build failed"; exit 1 }
}

$stage = Join-Path $Dist "GTX-Browser"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

# The installer expects both exes side by side, so ship them together.
$files = @(
    ".\target\release\gtx-browser.exe",
    ".\target\release\gtx-setup.exe"
)
foreach ($f in $files) {
    if (-not (Test-Path $f)) { Write-Output "missing $f -- run without -NoBuild"; exit 1 }
    Copy-Item $f $stage
}
Copy-Item .\README.md $stage

# Sanity: the staged files must be real PE images, not zero-byte leftovers.
Get-ChildItem $stage -Filter *.exe | ForEach-Object {
    if ($_.Length -lt 32KB) {
        Write-Output "$($_.Name) is suspiciously small ($($_.Length) bytes)"
        exit 1
    }
    Write-Output ("  {0,-18} {1,10:N0} bytes" -f $_.Name, $_.Length)
}

$zip = Join-Path $Dist "GTX-Browser-$ver.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip

Write-Output ""
Write-Output "staged: $stage"
Write-Output "zip:    $zip"
Write-Output ""
Write-Output "Install:  run gtx-setup.exe next to gtx-browser.exe"
Write-Output "Portable: just run gtx-browser.exe from the folder"
