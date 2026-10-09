# Offline acceptance for the PowerShell bootstrap updater; run on Windows CI.
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$root = Join-Path ([IO.Path]::GetTempPath()) ("micu-update-test-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $root | Out-Null
$fixtureBinary = (Resolve-Path (Join-Path $PSScriptRoot "..\target\release\micu-image-mcp.exe")).Path
$fixtureVersion = (& $fixtureBinary version).Trim()
$fixtureTag = "v$fixtureVersion"
$fixturePrerelease = $false
$fixtureCorrupt = $false
$fixtureSums = Join-Path $root "SHA256SUMS"
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $fixtureBinary).Hash
Set-Content -LiteralPath $fixtureSums -Value "$hash  micu-image-mcp-windows-x86_64.exe" -Encoding ASCII
function Invoke-RestMethod {
    param($Uri, $Headers, $TimeoutSec)
    if ($Uri -ne "https://api.github.com/repos/Subaru486desuwa/micu-image-mcp/releases/latest") { throw "Unexpected metadata URL" }
    return [pscustomobject]@{ tag_name = $fixtureTag; draft = $false; prerelease = $fixturePrerelease }
}
function Invoke-WebRequest {
    param([switch]$UseBasicParsing, $Uri, $OutFile, $TimeoutSec)
    $base = "https://github.com/Subaru486desuwa/micu-image-mcp/releases/download/$fixtureTag"
    if ($Uri -eq "$base/SHA256SUMS") { Copy-Item -LiteralPath $fixtureSums -Destination $OutFile }
    elseif ($Uri -eq "$base/micu-image-mcp-windows-x86_64.exe") {
        if ($fixtureCorrupt) { [IO.File]::WriteAllText($OutFile, "corrupt download") }
        else { Copy-Item -LiteralPath $fixtureBinary -Destination $OutFile }
    }
    else { throw "Unexpected asset URL" }
}
function Assert-RejectedUpdate {
    param([string]$Label, [string]$Message)
    $failed = $false
    try { & $updater -BinaryPath $target }
    catch {
        if ($_.Exception.Message -notmatch $Message) { throw "${Label}: unexpected failure: $($_.Exception.Message)" }
        $failed = $true
    }
    if (-not $failed) { throw "${Label}: unsafe update was accepted" }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash -ne $hash) { throw "${Label}: installed binary changed" }
    if ([IO.File]::ReadAllText("$target.previous") -ne "old binary") { throw "${Label}: backup changed" }
    if ([IO.File]::ReadAllText($config) -ne "custom save root") { throw "${Label}: configuration changed" }
    if ([IO.File]::ReadAllText($session) -ne "prior image context") { throw "${Label}: session changed" }
    if (Test-Path -LiteralPath "$target.update-lock") { throw "${Label}: update lock leaked" }
    if (@(Get-ChildItem -LiteralPath $root -Directory -Filter '.micu-update-*').Count) { throw "${Label}: temporary download directory leaked" }
    Write-Host "Passed: $Label"
}
try {
    $target = Join-Path $root "installed it's binary.exe"
    $config = Join-Path $root "client-config.json"
    $session = Join-Path $root "image-session.json"
    [IO.File]::WriteAllText($target, "old binary")
    [IO.File]::WriteAllText($config, "custom save root")
    [IO.File]::WriteAllText($session, "prior image context")
    $updater = Join-Path $PSScriptRoot "..\scripts\update.ps1"
    & $updater -BinaryPath $target
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash -ne $hash) { throw "Updated binary mismatch" }
    if ([IO.File]::ReadAllText("$target.previous") -ne "old binary") { throw "Backup mismatch" }
    if ([IO.File]::ReadAllText($config) -ne "custom save root") { throw "Configuration changed" }
    if ([IO.File]::ReadAllText($session) -ne "prior image context") { throw "Session changed" }
    $fixtureCorrupt = $true
    Assert-RejectedUpdate "corrupt download" "SHA-256 verification failed"
    $fixtureCorrupt = $false
    Add-Content -LiteralPath $fixtureSums -Value "$hash  micu-image-mcp-windows-x86_64.exe" -Encoding ASCII
    Assert-RejectedUpdate "duplicate checksum entries" "exactly one entry"
    Set-Content -LiteralPath $fixtureSums -Value "$hash  micu-image-mcp-windows-x86_64.exe" -Encoding ASCII
    $fixtureTag = "v999.0.0"
    Assert-RejectedUpdate "release and binary version mismatch" "version does not match"
    $fixtureTag = "v$fixtureVersion"
    $fixturePrerelease = $true
    Assert-RejectedUpdate "prerelease rejection" "not a stable version"
    $fixturePrerelease = $false

    $held = [IO.File]::Open($target, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try { Assert-RejectedUpdate "locked Windows executable" "Could not replace the installed EXE" }
    finally { $held.Dispose() }

    $existingLock = "$target.update-lock"
    New-Item -ItemType Directory -Path $existingLock | Out-Null
    $failed = $false
    try { & $updater -BinaryPath $target } catch { $failed = $true }
    if (-not $failed -or -not (Test-Path -LiteralPath $existingLock)) { throw "Concurrent update removed another updater's lock" }
    Remove-Item -LiteralPath $existingLock
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash -ne $hash) { throw "Concurrent update changed installed binary" }
    & $updater -BinaryPath $target
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath "$target.previous").Hash -ne $hash) { throw "Repeated update backup mismatch" }
    Write-Host "PowerShell update acceptance passed: replacement, repeated update, backup, preserved user files, corruption/duplicate/version/prerelease/locked-file/concurrent-update rejection."
}
finally { Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue }
