# Install a SHA-256-pinned NuDox Windows archive for the current user.
# The expected digest must come from a separately trusted release manifest.
[CmdletBinding(DefaultParameterSetName = 'Install')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Install')][string]$Archive,
    [Parameter(Mandatory, ParameterSetName = 'Install')][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$Sha256,
    [Parameter(Mandatory, ParameterSetName = 'Uninstall')][switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is unavailable.' }
if (-not $Uninstall) { Write-Warning 'This Windows release is unsigned. Windows may show publisher or trust warnings.' }
$parent = Join-Path $env:LOCALAPPDATA 'Programs'
$destination = Join-Path $parent 'Nudox'
$markerName = '.nudox-installed-files.json'
if ($Uninstall) {
    $destinationItem = Get-Item -LiteralPath $destination -Force
    if (($destinationItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Installation directory is a reparse point.' }
    $marker = Join-Path $destination $markerName
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) { throw 'No NuDox installation marker found.' }
    $manifest = Get-Content -Raw -LiteralPath $marker | ConvertFrom-Json
    if ($manifest.schema -ne 1 -or -not $manifest.files) { throw 'Installation marker is invalid.' }
    $expected = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    [void]$expected.Add($markerName)
    foreach ($entry in $manifest.files) {
        if ($entry -notmatch '^(bin/[A-Za-z0-9_.+\-]+|resources/build-manifest\.json|install-windows\.ps1|README\.txt)$') { throw 'Installation marker contains an unsafe path.' }
        [void]$expected.Add(($entry -replace '/', '\'))
    }
    $files = @(Get-ChildItem -LiteralPath $destination -File -Recurse -Force)
    foreach ($file in $files) {
        if (($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Installation contains a reparse point.' }
        $relative = [IO.Path]::GetRelativePath($destination, $file.FullName)
        if (-not $expected.Contains($relative)) { throw "Unexpected installed file: $relative" }
    }
    $dirs = @(Get-ChildItem -LiteralPath $destination -Directory -Recurse -Force)
    foreach ($dir in $dirs) {
        if (($dir.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Installation contains a reparse point.' }
    }
    Remove-Item -LiteralPath $destination -Recurse -Force
    Write-Output 'NuDox program files removed. User data was retained.'
    return
}
if (Test-Path -LiteralPath $destination) { throw 'NuDox is already installed; uninstall it before installing.' }
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$stage = Join-Path $parent ('.nudox-install-' + [guid]::NewGuid().ToString('N'))
$inputStream = [IO.File]::Open($Archive, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
try {
    if ($inputStream.Length -gt 2147483648) { throw 'Archive exceeds the size limit.' }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $actual = [Convert]::ToHexString($sha.ComputeHash($inputStream)).ToLowerInvariant() } finally { $sha.Dispose() }
    if ($actual -ne $Sha256.ToLowerInvariant()) { throw 'Archive SHA-256 does not match the supplied digest.' }
    $inputStream.Position = 0
    $zip = [IO.Compression.ZipArchive]::new($inputStream, [IO.Compression.ZipArchiveMode]::Read, $true)
    try {
        $seen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $files = [System.Collections.Generic.List[string]]::new()
        $total = [long]0
        foreach ($entry in $zip.Entries) {
            if ($entry.FullName.EndsWith('/')) { continue }
            if ($entry.FullName -notmatch '^Nudox/(bin/[A-Za-z0-9_.+\-]+|resources/build-manifest\.json|install-windows\.ps1|README\.txt)$') { throw "Unsafe archive entry: $($entry.FullName)" }
            $relative = $Matches[1]
            if (-not $seen.Add($relative)) { throw 'Archive has duplicate or case-ambiguous paths.' }
            $total += $entry.Length
            if ($entry.Length -gt 1073741824 -or $total -gt 2147483648 -or $seen.Count -gt 10000) { throw 'Archive exceeds extraction limits.' }
            $out = Join-Path $stage ($relative -replace '/', '\')
            New-Item -ItemType Directory -Path (Split-Path -Parent $out) -Force | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $out, $false)
            $files.Add($relative)
        }
        foreach ($name in @('bin/backend-desktop.exe', 'bin/backend-cli.exe', 'bin/backend-mcp.exe', 'bin/backend-locald.exe', 'bin/backend-metrics.exe', 'bin/backend-stylo-worker.exe', 'bin/backend-sqlite-worker.exe', 'resources/build-manifest.json')) {
            if (-not $seen.Contains($name)) { throw "Archive omits required file: $name" }
        }
        @{ schema = 1; archive_sha256 = $actual; files = @($files) } | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $stage $markerName) -Encoding utf8
        [IO.Directory]::Move($stage, $destination)
        Write-Output "Installed NuDox to $destination"
    } finally { $zip.Dispose() }
} finally {
    $inputStream.Dispose()
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
}
