# Copies Theater Reader into OBS's plugin folders (per machine and per user).
# Close OBS first.
#
# It ships its own copy of Theater Scene's Qt shim (trust_qt.dll), for its
# item in the 🎭 Theater menu: built by Theater Scene's Trust\build_trust_qt.ps1.
# Usage: cargo build --release; .\deploy.ps1 [-QtShim <path to trust_qt.dll>]
param([string]$QtShim = "$PSScriptRoot\..\theater scene(rust)\Trust\target\release\trust_qt.dll")
$ErrorActionPreference = 'Stop'

$dll = "$PSScriptRoot\target\release\theater_reader.dll"
if (!(Test-Path $dll)) { throw "$dll not found - run cargo build --release first." }
if (Get-Process obs64 -ErrorAction SilentlyContinue) { throw 'OBS is running - close it first.' }
if (!(Test-Path $QtShim)) { Write-Host "No Qt shim at $($QtShim): the menu item goes in OBS's Tools menu instead." }

foreach ($root in @("C:\ProgramData\obs-studio\plugins\theater-reader", "$env:APPDATA\obs-studio\plugins\theater-reader")) {
    New-Item -ItemType Directory -Path "$root\bin\64bit" -Force | Out-Null
    New-Item -ItemType Directory -Path "$root\data" -Force | Out-Null
    Copy-Item $dll "$root\bin\64bit\theater-reader.dll" -Force
    if (Test-Path $QtShim) { Copy-Item $QtShim "$root\bin\64bit\trust_qt.dll" -Force }
    Copy-Item "$PSScriptRoot\assets\icon.png" "$root\data\icon.png" -Force
}
Write-Host "=== C:\ProgramData\obs-studio\plugins\theater-reader ==="
Get-ChildItem -Path "C:\ProgramData\obs-studio\plugins\theater-reader" -Recurse -File | Format-Table FullName, LastWriteTime, Length
