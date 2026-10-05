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
# The version, which the Theater Installer reads to offer updates
Set-Content -LiteralPath "$stage\version.txt" -Value $Version -Encoding ascii

$zip = "$PSScriptRoot\dist\theater-reader-$Version-windows-x64.zip"
if (Test-Path $zip) { Remove-Item $zip }
# Entries named with / between folders, as zip tools expect (in Windows
# PowerShell, Compress-Archive and ZipFile.CreateFromDirectory put \)
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::Open($zip, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    $base = $work.Length + 1
    foreach ($file in Get-ChildItem -LiteralPath $stage -Recurse -File) {
        $name = $file.FullName.Substring($base).Replace('\', '/')
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName, $name, [System.IO.Compression.CompressionLevel]::Optimal)
    }
} finally { $archive.Dispose() }
Remove-Item $work -Recurse -Force
Write-Host "Made $zip"
