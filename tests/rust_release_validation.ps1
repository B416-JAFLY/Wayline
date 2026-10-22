# Isolated package/configuration checks; no service, firewall or radio operations.
$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/release-common.ps1')
$root = Join-Path ([IO.Path]::GetTempPath()) ('omdrop-rust-release-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
function Expect-Failure([scriptblock]$Action) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    if (-not $failed) { throw 'Expected failure.' }
}
function Write-Manifest($Manifest, [string]$Package) {
    [IO.File]::WriteAllText((Join-Path $Package 'release.json'), ($Manifest | ConvertTo-Json -Depth 8))
}
try {
    $package = Join-Path $root 'package'
    $install = Join-Path $root 'installed'
    $config = Join-Path $root 'config.json'
    New-Item -ItemType Directory -Path $package, $install | Out-Null
    $names = @('omdrop-service.exe','omdrop-bridge.exe','wintun.dll','LICENSE','QUICKSTART.zh-CN.md',
        'licenses/dependencies.json','licenses/owly-LICENSE.txt','licenses/wintun-LICENSE.txt',
        'vcruntime140.dll','msvcp140.dll','licenses/visual-cpp-Redist.txt','install.cmd',
        'scripts/install-prebuilt.ps1','scripts/release-common.ps1','scripts/set-service-access.ps1')
    foreach ($name in $names) {
        $path = Join-Path $package $name
        New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
        [IO.File]::WriteAllText($path, "fixture $name")
    }
    $record = @{
        schemaVersion = 1; profile = 'release'; sourceRevision = ('a' * 40)
        upstreamRevision = '2e9b2b0228432d5e402a9c90a031b419960d0bd1'
        executableSha256 = (Get-FileHash (Join-Path $package 'omdrop-bridge.exe')).Hash
        wintunSha256 = (Get-FileHash (Join-Path $package 'wintun.dll')).Hash
    }
    [IO.File]::WriteAllText((Join-Path $package 'omdrop-bridge-build.json'), ($record | ConvertTo-Json))
    $entries = @()
    foreach ($name in @($names) + @('omdrop-bridge-build.json')) {
        $path = Join-Path $package $name
        $entries += @{path=$name;length=(Get-Item $path).Length;sha256=(Get-FileHash $path).Hash}
    }
    $bridge = @{
        sourceRevision=$record.sourceRevision;upstreamRevision=$record.upstreamRevision
        executableSha256=$record.executableSha256;wintunSha256=$record.wintunSha256
        buildRecordSha256=(Get-FileHash (Join-Path $package 'omdrop-bridge-build.json')).Hash
    }
    $manifest = @{schemaVersion=2;platform='windows-x64';runtime='Rust';version='test';sourceRevision=('a'*40);radioBackend='rust';bridge=$bridge;files=$entries}
    Write-Manifest $manifest $package
    $null = Test-ReleasePackage $package
    if (Test-Path (Join-Path $package 'owl.exe')) { throw 'Rust fixture must not need owl.exe.' }
    $bridge.executableSha256 = ('0' * 64)
    Write-Manifest $manifest $package
    Expect-Failure { Test-ReleasePackage $package }
    $bridge.executableSha256 = $record.executableSha256
    $manifest.files = @($entries | Where-Object path -ne 'omdrop-bridge.exe')
    Write-Manifest $manifest $package
    Expect-Failure { Test-ReleasePackage $package }
    $manifest.files = $entries
    $manifest.radioBackend = 'owl'
    Write-Manifest $manifest $package
    Expect-Failure { Test-ReleasePackage $package }
    $manifest.radioBackend = 'rust'
    Write-Manifest $manifest $package
    $null = Test-ReleasePackage $package

    Assert-InstallBridgeConfiguration $config $package $install
    [IO.File]::WriteAllText($config, '{"port":"COM4","unusedSetting":true}')
    Assert-InstallBridgeConfiguration $config $package $install
    [IO.File]::WriteAllText($config, '{"bridgePath":"relative.exe"}')
    Expect-Failure { Assert-InstallBridgeConfiguration $config $package $install }
    $custom = Join-Path $root 'custom-bridge.exe'
    [IO.File]::WriteAllText($custom, 'custom bridge')
    [IO.File]::WriteAllText($config, (@{bridgePath=$custom} | ConvertTo-Json))
    Assert-InstallBridgeConfiguration $config $package $install
    [IO.File]::WriteAllText($config, (@{bridgePath=(Join-Path $install 'missing.exe')} | ConvertTo-Json))
    Expect-Failure { Assert-InstallBridgeConfiguration $config $package $install }
    [IO.File]::WriteAllText($config, (@{bridgePath=(Join-Path $install 'omdrop-bridge.exe')} | ConvertTo-Json))
    Assert-InstallBridgeConfiguration $config $package $install
    Write-Host 'Rust release validation passed: default bridge, provenance, missing binaries and bridge override preflight.'
} finally {
    # The resolved target is the unique temporary directory created above.
    $resolved = [IO.Path]::GetFullPath($root)
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if ($resolved.StartsWith($temp, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $resolved) -like 'omdrop-rust-release-*') {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
