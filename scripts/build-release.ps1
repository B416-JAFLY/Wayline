# Build matching host executables and record their provenance for packaging.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'release-common.ps1')
& (Join-Path $PSScriptRoot 'build-bridge.ps1')
& (Join-Path $PSScriptRoot 'build-desktop.ps1')
& cargo build --manifest-path (Join-Path $root 'Cargo.toml') --release --locked -p omdrop-cli -p omdrop-service
if ($LASTEXITCODE) { throw 'CLI/service release build failed.' }
$sourceRevision = & git -C $root rev-parse HEAD
if ($LASTEXITCODE) { throw 'Cannot record host source revision.' }
$entries = @()
foreach ($name in @('omdrop-desktop', 'omdrop-cli', 'omdrop-service', 'omdrop-bridge')) {
    $path = Join-Path $root "target/release/$name.exe"
    $version = Get-ExecutableVersion $path
    $versionMatch = [regex]::Match($version, "^$name ([0-9]+\.[0-9]+\.[0-9]+)$")
    if (-not $versionMatch.Success) { throw "Invalid version from $name" }
    $entries += [ordered]@{ path = "$name.exe"; version = $versionMatch.Groups[1].Value; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$record = [ordered]@{ schemaVersion = 2; sourceRevision = $sourceRevision; radioBackend = 'rust'; bridge = (Get-BridgeReleaseMetadata $root); airdrop = (Get-AirDropReleaseMetadata $root); files = $entries }
[IO.File]::WriteAllText((Join-Path $root 'target/release/omdrop-build.json'), ($record | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
Write-Host 'Host release binaries and provenance record are ready.'
