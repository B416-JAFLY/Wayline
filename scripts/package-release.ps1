# Assemble a complete offline package from checked, prebuilt inputs. Does not install.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$FirmwarePackage,
    [string]$ToolDirectory,
    [ValidateSet('Desktop', 'Rust', 'Cli')][string]$Runtime = 'Desktop',
    [switch]$IncludeCli,
    [string]$VCRuntimeDirectory,
    [string]$VCRuntimeNotice,
    [string]$Version = '0.1.0'
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-common.ps1')
$root = Split-Path -Parent $PSScriptRoot
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Output directory already exists; choose a new directory.' }
$files = @{}
$generated = @{}
$dependencies = New-Object 'System.Collections.Generic.List[object]'
$hostRecord = Get-Content -LiteralPath (Join-Path $root 'target/release/omdrop-build.json') -Raw | ConvertFrom-Json
if ($hostRecord.schemaVersion -ne 2 -or $hostRecord.radioBackend -ne 'rust') { throw 'Run scripts/build-release.ps1 before packaging.' }
$airdrop = Get-AirDropReleaseMetadata $root
if (($airdrop | ConvertTo-Json -Depth 6 -Compress) -cne ($hostRecord.airdrop | ConvertTo-Json -Depth 6 -Compress)) {
    throw 'AirDrop protocol sources changed after the recorded build; rebuild the release.'
}
$generated['licenses/owly-airdrop-source.json'] = $airdrop | ConvertTo-Json -Depth 6
function Add-ReleaseFile([string]$Source, [string]$Target) {
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw "Missing release input: $Source" }
    if ($files.ContainsKey($Target)) { throw "Duplicate release target: $Target" }
    $files[$Target] = (Resolve-Path -LiteralPath $Source).Path
}
function Add-ReleaseTree([string]$Source, [string]$Target) {
    $base = (Resolve-Path -LiteralPath $Source).Path.TrimEnd('\', '/')
    foreach ($file in Get-PackageFiles $base) {
        $relative = $file.FullName.Substring($base.Length + 1).Replace('\', '/')
        $checked = Resolve-PackageFile $base $relative
        Add-ReleaseFile $checked "$Target/$relative"
    }
}
function Add-CargoLicenses([string]$ManifestPath, [string]$Scope, [string]$Platform) {
    $arguments = @('metadata', '--manifest-path', $ManifestPath, '--locked', '--offline', '--format-version', '1')
    if ($Platform) { $arguments += @('--filter-platform', $Platform) }
    $json = & cargo @arguments
    if ($LASTEXITCODE) { throw "Cannot collect $Scope dependency licenses from the locked offline metadata." }
    $metadata = $json | ConvertFrom-Json
    foreach ($package in $metadata.packages | Sort-Object name, version) {
        $base = Split-Path -Parent $package.manifest_path
        $texts = @{}
        foreach ($license in Get-ChildItem -LiteralPath $base -File -Force | Where-Object { $_.Name -match '^(?i:LICEN[CS]E|COPYING|NOTICE)(?:[._-]|$)' }) {
            $texts[$license.FullName] = $license.Name
        }
        if ($package.license_file) {
            $explicit = [IO.Path]::GetFullPath((Join-Path $base $package.license_file))
            if (-not (Test-Path -LiteralPath $explicit -PathType Leaf)) { throw "Missing declared license for $($package.name): $explicit" }
            $texts[$explicit] = Split-Path -Leaf $explicit
        }
        $licenseTargets = @()
        foreach ($source in $texts.Keys | Sort-Object) {
            $target = "licenses/$Scope/$($package.name)-$($package.version)/$($texts[$source])"
            Add-ReleaseFile $source $target
            $licenseTargets += $target
        }
        $dependencies.Add([ordered]@{ scope = $Scope; name = $package.name; version = $package.version; license = $package.license; source = $package.source; texts = $licenseTargets })
    }
}

$binary = switch ($Runtime) { 'Desktop' { 'omdrop-desktop' } 'Rust' { 'omdrop-service' } 'Cli' { 'omdrop-cli' } }
$binaries = @($binary)
if ($Runtime -ne 'Cli') { $binaries += 'omdrop-bridge' }
if ($IncludeCli -and $Runtime -ne 'Cli') { $binaries += 'omdrop-cli' }
foreach ($name in $binaries) {
    $source = Join-Path $root "target/release/$name.exe"
    Add-ReleaseFile $source "$name.exe"
    $actualVersion = Get-ExecutableVersion $source
    if ($actualVersion -ne "$name $Version") { throw "Mismatched $name version; build the current release binaries first." }
    $recorded = @($hostRecord.files | Where-Object path -eq "$name.exe")
    if ($recorded.Count -ne 1 -or $recorded[0].version -ne $Version -or
        (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ine $recorded[0].sha256) { throw "Host binary changed after the recorded build: $name" }
}
Add-ReleaseFile (Join-Path $root 'LICENSE') 'LICENSE'
Add-ReleaseFile (Join-Path $root 'docs/QUICKSTART.zh-CN.md') 'QUICKSTART.zh-CN.md'
Add-CargoLicenses (Join-Path $root 'Cargo.toml') 'host-rust' 'x86_64-pc-windows-msvc'
# The x64 MSVC runtime is app-local so a clean Windows machine needs no VC installer.
if (-not $VCRuntimeDirectory) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($LASTEXITCODE) { throw 'Cannot locate the Visual C++ runtime.' }
        if ($vsRoot) {
            $candidate = Get-ChildItem -Path (Join-Path $vsRoot 'VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT') -Directory |
                Sort-Object FullName -Descending | Select-Object -First 1
            if ($candidate) { $VCRuntimeDirectory = $candidate.FullName }
        }
    }
}
if (-not $VCRuntimeDirectory) { throw 'Specify -VCRuntimeDirectory with the Visual Studio x64 Microsoft.VC*.CRT directory.' }
$VCRuntimeDirectory = (Resolve-Path -LiteralPath $VCRuntimeDirectory).Path
if (-not $VCRuntimeNotice) {
    $ancestor = $VCRuntimeDirectory
    while ($ancestor) {
        $notices = @(Get-ChildItem -Path (Join-Path $ancestor 'Licenses/*/Redist.txt') -File -ErrorAction SilentlyContinue)
        if ($notices.Count) { $VCRuntimeNotice = $notices[0].FullName; break }
        $ancestor = Split-Path -Parent $ancestor
    }
}
if (-not $VCRuntimeNotice) { throw 'Specify -VCRuntimeNotice with the matching Visual Studio Redist.txt.' }
foreach ($required in @('vcruntime140.dll', 'msvcp140.dll')) {
    if (-not (Test-Path -LiteralPath (Join-Path $VCRuntimeDirectory $required) -PathType Leaf)) { throw "Missing x64 VC runtime: $required" }
}
foreach ($dll in Get-ChildItem -LiteralPath $VCRuntimeDirectory -Filter '*.dll' -File) {
    $stream = [IO.File]::OpenRead($dll.FullName)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($reader.ReadUInt16() -ne 0x5a4d) { throw "Invalid runtime DLL: $($dll.Name)" }
        $stream.Position = 0x3c
        $header = $reader.ReadUInt32()
        $stream.Position = $header
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) { throw "VC runtime must contain x64 PE files: $($dll.Name)" }
    } finally { $reader.Dispose() }
    Add-ReleaseFile $dll.FullName $dll.Name
}
Add-ReleaseFile $VCRuntimeNotice 'licenses/visual-cpp-Redist.txt'
$vcVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo((Join-Path $VCRuntimeDirectory 'vcruntime140.dll')).FileVersion
$dependencies.Add([ordered]@{ scope = 'host'; name = 'Microsoft Visual C++ Runtime'; version = $vcVersion; license = $null; source = $null; texts = @('licenses/visual-cpp-Redist.txt') })
$bridge = $null
if ($Runtime -ne 'Cli') {
Add-ReleaseFile (Join-Path $root 'target/release/wintun.dll') 'wintun.dll'
Add-ReleaseFile (Join-Path $root 'target/release/omdrop-bridge-build.json') 'omdrop-bridge-build.json'
Add-ReleaseFile (Join-Path $root 'vendor/owly-awdl/LICENSE') 'licenses/owly-LICENSE.txt'
foreach ($script in @('install-prebuilt.ps1', 'release-common.ps1', 'set-service-access.ps1')) { Add-ReleaseFile (Join-Path $PSScriptRoot $script) "scripts/$script" }
Add-ReleaseFile (Join-Path $PSScriptRoot 'install.cmd') 'install.cmd'
$bridge = Get-BridgeReleaseMetadata $root
if (($bridge | ConvertTo-Json -Depth 6 -Compress) -cne ($hostRecord.bridge | ConvertTo-Json -Depth 6 -Compress)) {
    throw 'Rust bridge changed after the recorded host build; rebuild the matching release.'
}
Add-ReleaseFile (Join-Path $root 'target/release/bridge-licenses/Wintun-LICENSE.txt') 'licenses/wintun-LICENSE.txt'
}
$firmware = $null
$tool = $null
if ($Runtime -eq 'Desktop') {
    if (-not $FirmwarePackage -or -not $ToolDirectory) { throw 'Desktop packaging requires -FirmwarePackage and -ToolDirectory.' }
    $firmware = Test-FirmwarePackage $FirmwarePackage
    $tool = Get-Content -LiteralPath (Join-Path $ToolDirectory 'tool.json') -Raw | ConvertFrom-Json
    if ($firmware.formatVersion -ne 1 -or $firmware.board -ne 'cardputer-adv' -or $firmware.chip -ne 'esp32s3' -or
        $firmware.flashSize -ne 8388608 -or $firmware.protocolVersion -ne 2 -or $firmware.idfVersion -ne '6.0' -or
        $firmware.toolVersion -ne '5.0.2' -or $tool.version -ne '5.0.2' -or
        $tool.executableSha256 -ne 'ec11178326ab909d6989ba51e98a68c363be6386f2c8f1f1a8a2cd9ca371e7e7' -or
        $tool.archiveSha256 -ne '1caef993a16c5915714a0da772d93f2e3239f316c06223981b262b838287268c') {
        throw 'Firmware and flash tool are not the supported pinned versions.'
    }
    foreach ($entry in $firmware.images) {
        $image = Resolve-PackageFile $FirmwarePackage $entry.path
        if ((Get-Item -LiteralPath $image).Length -ne $entry.length -or
            (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash -ine $entry.sha256) { throw "Firmware checksum mismatch: $($entry.path)" }
    }
    if ((Get-FileHash -LiteralPath (Join-Path $ToolDirectory 'esptool.exe') -Algorithm SHA256).Hash -ine $tool.executableSha256) { throw 'Flash tool checksum mismatch.' }
    foreach ($required in @('LICENSE.esptool', 'esptool-source.zip')) {
        if (-not (Test-Path -LiteralPath (Join-Path $ToolDirectory $required) -PathType Leaf)) { throw "Missing esptool distribution file: $required" }
    }
    if ((Get-FileHash -LiteralPath (Join-Path $ToolDirectory 'LICENSE.esptool') -Algorithm SHA256).Hash -ine '8177f97513213526df2cf6184d8ff986c675afb514d4e68a404010521b880643' -or
        (Get-FileHash -LiteralPath (Join-Path $ToolDirectory 'esptool-source.zip') -Algorithm SHA256).Hash -ine 'df2ce32a46c091fb2f1358bb37a2ea5144253862d82d0c9872df07ccb9e7e436') {
        throw 'esptool license/source checksum mismatch.'
    }
    Add-ReleaseTree $FirmwarePackage 'firmware'
    Add-ReleaseTree $ToolDirectory 'tools/esptool'
    Add-ReleaseFile (Join-Path $root 'firmware/assets/OFL.txt') 'licenses/firmware-font-OFL.txt'
    Add-CargoLicenses (Join-Path $root 'firmware/Cargo.toml') 'firmware-rust' ''
    $project = Get-Content -LiteralPath (Join-Path $root 'firmware/build-rust/project_description.json') -Raw | ConvertFrom-Json
    if ($project.git_revision -notmatch '^v?6\.0(?:\D|$)') { throw 'Firmware license inventory requires the matching ESP-IDF 6.0 build.' }
    Add-ReleaseFile (Join-Path $project.idf_path 'LICENSE') 'licenses/esp-idf/LICENSE'
    foreach ($component in $project.build_component_paths | Where-Object { $_ } | Sort-Object -Unique) {
        $componentName = Split-Path -Leaf $component
        foreach ($license in Get-ChildItem -LiteralPath $component -File -Recurse | Where-Object { $_.Name -match '^(?i:LICEN[CS]E|COPYING|NOTICE)(?:[._-]|$)' }) {
            $relative = $license.FullName.Substring([IO.Path]::GetFullPath($component).TrimEnd('\', '/').Length + 1).Replace('\', '/')
            Add-ReleaseFile $license.FullName "licenses/firmware-components/$componentName/$relative"
        }
    }
    Add-ReleaseFile (Join-Path $root 'firmware/dependencies.lock') 'licenses/firmware-components.lock'
}
$sourceRevision = & git -C $root rev-parse HEAD
if ($LASTEXITCODE) { throw 'Cannot read host source revision.' }
if ($sourceRevision -ne $hostRecord.sourceRevision) { throw 'Host binaries belong to another source revision; rebuild first.' }
if ($firmware -and $firmware.sourceRevision -ne $sourceRevision) { throw 'Firmware and host source revisions do not match.' }
$generated['licenses/dependencies.json'] = [ordered]@{ schemaVersion = 1; dependencies = $dependencies.ToArray() } | ConvertTo-Json -Depth 8
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$entries = @()
foreach ($relative in $files.Keys | Sort-Object) {
    $target = Join-Path $OutputDirectory $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
    Copy-Item -LiteralPath $files[$relative] -Destination $target
    $entries += [ordered]@{ path = $relative; length = (Get-Item -LiteralPath $target).Length; sha256 = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() }
}
foreach ($relative in $generated.Keys | Sort-Object) {
    $target = Join-Path $OutputDirectory $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
    [IO.File]::WriteAllText($target, $generated[$relative], [Text.UTF8Encoding]::new($false))
    $entries += [ordered]@{ path = $relative; length = (Get-Item -LiteralPath $target).Length; sha256 = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$manifest = [ordered]@{ schemaVersion = 2; platform = 'windows-x64'; runtime = $Runtime; version = $Version; sourceRevision = $sourceRevision; radioBackend = 'rust'; bridge = $bridge; vcRuntimeVersion = $vcVersion; firmwareVersion = $(if ($firmware) { $firmware.firmwareVersion } else { $null }); toolVersion = $(if ($tool) { $tool.version } else { $null }); files = $entries }
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'release.json'), ($manifest | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
$null = Test-ReleasePackage $OutputDirectory
Write-Host "Offline package ready: $OutputDirectory"
