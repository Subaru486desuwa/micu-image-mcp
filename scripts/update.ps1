param([string]$BinaryPath, [switch]$PreflightOnly)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ($PSVersionTable.PSVersion.Major -lt 6) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}
$architecture = $env:PROCESSOR_ARCHITEW6432
if ([string]::IsNullOrWhiteSpace($architecture)) { $architecture = $env:PROCESSOR_ARCHITECTURE }
if ($architecture -ine "AMD64") { throw "No Windows release binary for $architecture; use the README source build instructions." }
if ($PreflightOnly) { Write-Host "micu-image-mcp updater preflight: Windows x86_64 supported."; return }
if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
    $BinaryPath = Join-Path $env:LOCALAPPDATA "micu-image-mcp\bin\micu-image-mcp.exe"
}
$target = Get-Item -LiteralPath $BinaryPath
if ($target.PSIsContainer -or ($target.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Update target must be an installed regular file."
}
$lock = "$($target.FullName).update-lock"
New-Item -ItemType Directory -Path $lock -ErrorAction Stop | Out-Null
$tempDir = $null
try {
    $tempDir = Join-Path $target.DirectoryName (".micu-update-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tempDir | Out-Null
    $repo = "Subaru486desuwa/micu-image-mcp"
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers @{ "User-Agent" = "micu-image-mcp-updater" } -TimeoutSec 20
    if ($release.draft -or $release.prerelease -or $release.tag_name -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
        throw "Latest release is not a stable version."
    }
    $tag = $release.tag_name
    $base = "https://github.com/$repo/releases/download/$tag"
    $asset = "micu-image-mcp-windows-x86_64.exe"
    $binary = Join-Path $tempDir $asset
    $checksums = Join-Path $tempDir "SHA256SUMS"
    Write-Host "Downloading $tag ($asset)..."
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $checksums -TimeoutSec 120
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $binary -TimeoutSec 120
    if ((Get-Item -LiteralPath $checksums).Length -gt 1048576 -or (Get-Item -LiteralPath $binary).Length -gt 134217728) {
        throw "Release asset exceeds size limit."
    }
    $lines = @(Get-Content -LiteralPath $checksums | Where-Object { $_ -match "\s+$([regex]::Escape($asset))$" })
    if ($lines.Count -ne 1) { throw "SHA256SUMS must contain exactly one entry for $asset." }
    $expected = ($lines[0] -split '\s+')[0]
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash
    if ($actual -ine $expected) { throw "SHA-256 verification failed; existing binary preserved." }
    $version = (& $binary version).Trim()
    if ($LASTEXITCODE -ne 0 -or "v$version" -ne $tag) { throw "Binary version does not match release tag." }
    $backup = "$($target.FullName).previous"
    # Disconnect MCP first on Windows if another process holds the installed EXE.
    try { [IO.File]::Replace($binary, $target.FullName, $backup) }
    catch { throw "Could not replace the installed EXE. Disconnect MCP and retry; existing binary is preserved. $($_.Exception.Message)" }
    Write-Host "Updated to $tag. Configuration, credentials, images and sessions preserved."
    Write-Host "Backup: $backup"
    Write-Host "Reconnect MCP, then confirm the version with server_info."
}
finally {
    if ($tempDir) { Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $lock -Force -ErrorAction SilentlyContinue
}
