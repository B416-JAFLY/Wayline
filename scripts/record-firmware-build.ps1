# Called only after a successful ESP-IDF build. Records the exact image set.
param(
    [Parameter(Mandatory = $true)][string]$BuildDirectory,
    [ValidateSet('esp32s3', 'esp32c5')]
    [string]$Target = 'esp32s3'
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-common.ps1')
$build = (Resolve-Path -LiteralPath $BuildDirectory).Path
$argsPath = Join-Path $build 'flasher_args.json'
$flash = Get-Content -LiteralPath $argsPath -Raw | ConvertFrom-Json
$project = Get-Content -LiteralPath (Join-Path $build 'project_description.json') -Raw | ConvertFrom-Json
if ($project.target -ne $Target -or $project.git_revision -notmatch '^v?6\.0(?:\D|$)') {
    throw "Only the requested ESP-IDF 6.0 $Target firmware build is supported."
}
if ($Target -eq 'esp32s3' -and $flash.flash_settings.flash_size -ne '8MB') {
    throw 'The ESP32-S3 Cardputer firmware must use an 8 MiB flash device.'
}
$sourceRevision = & git -C (Split-Path -Parent $PSScriptRoot) rev-parse HEAD
if ($LASTEXITCODE) { throw 'Cannot record firmware source revision.' }
$images = @()
foreach ($entry in $flash.flash_files.PSObject.Properties) {
    $path = Resolve-PackageFile $build $entry.Value
    $images += [ordered]@{ path = $entry.Value; offset = [Convert]::ToUInt32($entry.Name, 16); length = (Get-Item -LiteralPath $path).Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$record = [ordered]@{ formatVersion = 1; target = $project.target; flashSize = $flash.flash_settings.flash_size; sourceRevision = $sourceRevision; firmwareVersion = $project.project_version; idfVersion = '6.0'; argumentsSha256 = (Get-FileHash -LiteralPath $argsPath -Algorithm SHA256).Hash.ToLowerInvariant(); images = $images }
$json = $record | ConvertTo-Json -Depth 6
[IO.File]::WriteAllText((Join-Path $build 'omdrop-build.json'), $json, [Text.UTF8Encoding]::new($false))
