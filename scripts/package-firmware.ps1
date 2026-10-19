[CmdletBinding()]
param(
    [string]$BuildDirectory,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
if (-not $BuildDirectory) { $BuildDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) 'firmware\build-rust' }
. (Join-Path $PSScriptRoot 'release-common.ps1')
$build = (Resolve-Path -LiteralPath $BuildDirectory).Path
$recordPath = Join-Path $build 'omdrop-build.json'
if (-not (Test-Path -LiteralPath $recordPath)) { throw 'Run scripts/build-firmware.ps1 successfully before packaging.' }
$record = Get-Content -LiteralPath $recordPath -Raw | ConvertFrom-Json
$flash = Get-Content -LiteralPath (Join-Path $build 'flasher_args.json') -Raw | ConvertFrom-Json
if ($record.formatVersion -ne 1 -or $record.argumentsSha256 -ine (Get-FileHash -LiteralPath (Join-Path $build 'flasher_args.json') -Algorithm SHA256).Hash) {
    throw 'The flash arguments do not belong to the recorded successful build.'
}
if (@($record.images).Count -ne 3 -or @($flash.flash_files.PSObject.Properties).Count -ne 3) { throw 'Expected bootloader, partition table and application only.' }
$images = @()
$end = 0L
foreach ($entry in $record.images | Sort-Object offset) {
    $path = Resolve-PackageFile $build $entry.path
    if ((Get-Item -LiteralPath $path).Length -ne $entry.length -or
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256) { throw "Image changed after build: $($entry.path)" }
    $offset = [long]$entry.offset
    if ($offset -lt $end -or $entry.length -le 0 -or ($offset + $entry.length) -gt 8388608) { throw 'Invalid or overlapping firmware image ranges.' }
    $kind = switch ($offset) { 0 { 'bootloader' } 32768 { 'partition-table' } 65536 { 'application' } default { throw 'Unsupported firmware partition layout.' } }
    # Bootloader/table must never overwrite the NVS partition beginning at 0x9000.
    if (($kind -eq 'bootloader' -and $offset + $entry.length -gt 32768) -or
        ($kind -eq 'partition-table' -and $offset + $entry.length -gt 36864)) { throw 'Firmware image overlaps reserved data.' }
    $images += [ordered]@{ path = "$kind.bin"; offset = $offset; length = $entry.length; sha256 = $entry.sha256; kind = $kind; source = $path }
    $end = $offset + $entry.length
}
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Output directory already exists; choose a new directory.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
foreach ($entry in $images) {
    Copy-Item -LiteralPath $entry.source -Destination (Join-Path $OutputDirectory $entry.path)
    $entry.Remove('source')
}
$table = $images | Where-Object kind -eq 'partition-table'
$manifest = [ordered]@{ formatVersion = 1; board = 'cardputer-adv'; chip = 'esp32s3'; flashSize = 8388608; firmwareVersion = $record.firmwareVersion; sourceRevision = $record.sourceRevision; protocolVersion = 2; idfVersion = '6.0'; layoutId = $table.sha256; toolVersion = '5.0.2'; images = $images }
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'manifest.json'), ($manifest | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
$null = Test-FirmwarePackage $OutputDirectory
Write-Host "Firmware package: $OutputDirectory"
