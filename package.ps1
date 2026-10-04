# Builds the release zip, dist\theater-reader-<version>-windows-x64.zip:
#   theater-reader\bin\64bit   the plugin (theater-reader.dll) and Theater Scene's Qt shim (trust_qt.dll)
#   theater-reader\data        its icon
# People extract it into C:\ProgramData\obs-studio\plugins.
# Build first: cargo build --release (and Theater Scene's Trust\build_trust_qt.ps1 for the shim)
param([string]$Version = "0.1.0", [string]$QtShim = "$PSScriptRoot\..\theater scene(rust)\Trust\target\release\trust_qt.dll")
$ErrorActionPreference = 'Stop'

$work = "$PSScriptRoot\dist\stage"
$stage = "$work\theater-reader"
$dll = "$PSScriptRoot\target\release\theater_reader.dll"
foreach ($f in @($dll, $QtShim)) {
    if (!(Test-Path -LiteralPath $f)) { throw "$f not found - build it first." }
}
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory -Force "$stage\bin\64bit", "$stage\data" | Out-Null
Copy-Item -LiteralPath $dll "$stage\bin\64bit\theater-reader.dll"
Copy-Item -LiteralPath $QtShim "$stage\bin\64bit\trust_qt.dll"
Copy-Item -LiteralPath "$PSScriptRoot\assets\icon.png" "$stage\data\icon.png"
Copy-Item -LiteralPath "$PSScriptRoot\LICENSE" "$stage\LICENSE.txt"

$zip = "$PSScriptRoot\dist\theater-reader-$Version-windows-x64.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Remove-Item $work -Recurse -Force
Write-Host "Made $zip"
