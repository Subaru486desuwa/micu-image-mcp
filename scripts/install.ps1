param(
    [switch]$PreflightOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($PSVersionTable.PSVersion.Major -lt 6) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

$repo = "Subaru486desuwa/micu-image-mcp"
$latestBase = "https://github.com/$repo/releases/latest/download"
$asset = "micu-image-mcp-windows-x86_64.exe"

$architecture = $env:PROCESSOR_ARCHITEW6432
if ([string]::IsNullOrWhiteSpace($architecture)) {
    $architecture = $env:PROCESSOR_ARCHITECTURE
}
if ([string]::IsNullOrWhiteSpace($architecture)) {
    throw "micu-image-mcp installer: unable to determine native Windows architecture"
}
if ($architecture -ine "AMD64") {
    throw "micu-image-mcp installer: Windows release binaries currently support x86_64 only (detected $architecture)"
}

if ($PreflightOnly) {
    Write-Host "micu-image-mcp installer preflight: Windows x86_64 supported."
    return
}

$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("micu-image-mcp-" + [Guid]::NewGuid().ToString("N"))
$binary = Join-Path $tempDir $asset
$checksums = Join-Path $tempDir "SHA256SUMS"

New-Item -ItemType Directory -Path $tempDir | Out-Null

try {
    Write-Host "Downloading $asset..."
    Invoke-WebRequest -UseBasicParsing -Uri "$latestBase/SHA256SUMS" -OutFile $checksums
    Invoke-WebRequest -UseBasicParsing -Uri "$latestBase/$asset" -OutFile $binary

    $checksumLine = Get-Content -LiteralPath $checksums | Where-Object { $_ -match "\s+$([regex]::Escape($asset))$" } | Select-Object -First 1
    if (-not $checksumLine) {
        throw "micu-image-mcp installer: SHA256SUMS does not contain $asset"
    }

    $expected = ($checksumLine -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "micu-image-mcp installer: SHA-256 verification failed for $asset"
    }

    $version = (& $binary version).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "micu-image-mcp installer: downloaded binary failed its version check"
    }
    Write-Host "Verified micu-image-mcp v$version ($asset)."

    & $binary install --yes
    if ($LASTEXITCODE -ne 0) {
        throw "micu-image-mcp installer: built-in install command failed with exit code $LASTEXITCODE"
    }
}
finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
