<#
.SYNOPSIS
    Builds backend-desktop and packages it as a portable Windows folder/zip.

.DESCRIPTION
    Windows counterpart of package-macos.sh: no installer, no signing, just a
    self-contained folder that runs in place. Mirrors that script's shape
    (profile + destination arguments, build then copy).

.PARAMETER Profile
    "release" (default) or "debug".

.PARAMETER Destination
    Output folder. Defaults to target\Nudox-windows under the repo root.

.PARAMETER Zip
    Also produce Destination + ".zip" alongside the folder.
#>
param(
    [string]$Profile = "release",
    [string]$Destination = "",
    [switch]$Zip
)

$ErrorActionPreference = "Stop"

$root = (Get-Item (Join-Path $PSScriptRoot "..\..")).FullName
if ($Profile -eq "debug") {
    $cargoProfile = "dev"
    $targetProfile = "debug"
} else {
    $cargoProfile = $Profile
    $targetProfile = $Profile
}
if (-not $Destination) {
    $Destination = Join-Path $root "target\Nudox-windows"
}

$binary = Join-Path $root "target\$targetProfile\backend-desktop.exe"

& cargo build --manifest-path (Join-Path $root "Cargo.toml") --profile $cargoProfile -p backend-desktop
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if (Test-Path $Destination) { Remove-Item -Recurse -Force $Destination }
New-Item -ItemType Directory -Path $Destination | Out-Null
Copy-Item $binary (Join-Path $Destination "Nudox.exe")

@"
Nudox (portable build)
=======================

Run Nudox.exe directly - no installation needed.

Requirements:
- Windows 10/11, 64-bit.
- The Visual C++ Runtime (VCRUNTIME140.dll). Already present on most
  Windows machines and on any machine with Visual Studio or its Build
  Tools installed. If Nudox.exe fails to start with a missing-DLL error,
  install "Microsoft Visual C++ Redistributable" (x64) from
  https://aka.ms/vs/17/release/vc_redist.x64.exe

Notes:
- This is a portable build: it carries no installer, writes no registry
  entries, and can be deleted by removing this folder. Application data
  (indexed packages, session state) is still written under
  %LOCALAPPDATA%\Nudox, same as any build.
- The graph "anatomy" decoration on symbol pages needs the
  Nudox-Design-System source checkout to be present at the same relative
  path it was built from; it degrades gracefully (no crash) when absent,
  which is expected in a build copied away from the source tree.
"@ | Out-File -Encoding utf8 (Join-Path $Destination "README.txt")

Write-Output $Destination

if ($Zip) {
    $zipPath = "$Destination.zip"
    if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
    Compress-Archive -Path (Join-Path $Destination "*") -DestinationPath $zipPath
    Write-Output $zipPath
}
