# Build the Rust AWDL child. Does not open devices or change services.
[CmdletBinding()]
param([switch]$DebugBuild, [switch]$Test)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
if ($Test) {
    & cargo test --manifest-path $manifest -p omdrop-awdl -p omdrop-bridge --all-targets --locked
    if ($LASTEXITCODE) { throw 'Rust bridge tests failed.' }
    & cargo test --manifest-path (Join-Path $root 'vendor/owly-awdl/Cargo.toml') --all-features --locked
    if ($LASTEXITCODE) { throw 'Pinned AWDL tests failed.' }
}
$arguments = @('build', '--manifest-path', $manifest, '-p', 'omdrop-bridge', '--locked')
$profile = 'release'
if ($DebugBuild) { $profile = 'debug' } else { $arguments += '--release' }
& cargo @arguments
if ($LASTEXITCODE) { throw 'Rust bridge build failed.' }

# Revalidate the archive even when a previous OWL build populated the cache.
$cache = Join-Path $root '.cache/bridge-wintun'
New-Item -ItemType Directory -Path $cache -Force | Out-Null
$zip = Join-Path $cache 'wintun-0.14.1.zip'
$expected = '07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51'
if (-not (Test-Path -LiteralPath $zip -PathType Leaf)) {
    $existing = Join-Path $root '.cache/upstream/wintun-dl/wintun.zip'
    if (Test-Path -LiteralPath $existing -PathType Leaf) { Copy-Item -LiteralPath $existing -Destination $zip }
    else { Invoke-WebRequest -Uri 'https://www.wintun.net/builds/wintun-0.14.1.zip' -OutFile $zip -UseBasicParsing }
}
if ((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
    throw 'Wintun archive checksum mismatch.'
}
Expand-Archive -LiteralPath $zip -DestinationPath $cache -Force
$output = Join-Path $root "target/$profile"
Copy-Item -LiteralPath (Join-Path $cache 'wintun/bin/amd64/wintun.dll') -Destination $output -Force
$licenses = Join-Path $output 'bridge-licenses'
New-Item -ItemType Directory -Path $licenses -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $cache 'wintun/LICENSE.txt') -Destination (Join-Path $licenses 'Wintun-LICENSE.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'vendor/owly-awdl/LICENSE') -Destination (Join-Path $licenses 'Owly-LICENSE.txt') -Force
$revision = & git -C $root rev-parse HEAD
if ($LASTEXITCODE) { throw 'Cannot record source revision.' }
$sourceFiles = @()
foreach ($directory in @('apps/bridge', 'crates/omdrop-awdl', 'crates/omdrop-service', 'crates/omdrop-proto', 'crates/omdrop-radio', 'vendor/owly-awdl/src')) {
    $sourceFiles += @(Get-ChildItem -LiteralPath (Join-Path $root $directory) -File -Recurse)
}
foreach ($file in @('Cargo.toml', 'Cargo.lock', 'vendor/owly-awdl/Cargo.toml', 'scripts/build-bridge.ps1')) {
    $sourceFiles += Get-Item -LiteralPath (Join-Path $root $file)
}
$dirty = @(& git -C $root status --porcelain)
if ($LASTEXITCODE) { throw 'Cannot record source working-tree state.' }
$sources = @($sourceFiles | Sort-Object FullName | ForEach-Object {
    [ordered]@{ path = $_.FullName.Substring($root.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
})
$record = [ordered]@{
    schemaVersion = 1
    sourceRevision = $revision
    sourceTreeDirty = ($dirty.Count -ne 0)
    profile = $profile
    upstreamRevision = '2e9b2b0228432d5e402a9c90a031b419960d0bd1'
    cargoLockSha256 = (Get-FileHash -LiteralPath (Join-Path $root 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant()
    executableSha256 = (Get-FileHash -LiteralPath (Join-Path $output 'omdrop-bridge.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    wintunSha256 = (Get-FileHash -LiteralPath (Join-Path $output 'wintun.dll') -Algorithm SHA256).Hash.ToLowerInvariant()
    sources = $sources
}
[IO.File]::WriteAllText((Join-Path $output 'omdrop-bridge-build.json'), ($record | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
Write-Host (Join-Path $output 'omdrop-bridge.exe')
