# Tests only temporary files. Never installs, starts services or opens serial ports.
$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/release-common.ps1')
$directory = Join-Path ([IO.Path]::GetTempPath()) ("omdrop-release-test-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
function Expect-Failure([scriptblock]$Action) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    if (-not $failed) { throw 'Expected validation to reject this input.' }
}
try {
    $names = @('omdrop-cli.exe', 'LICENSE', 'scripts/set-service-access.ps1',
        'scripts/install-prebuilt.ps1', 'scripts/release-common.ps1', 'install.cmd', 'QUICKSTART.zh-CN.md',
        'licenses/dependencies.json', 'vcruntime140.dll', 'msvcp140.dll', 'licenses/visual-cpp-Redist.txt')
    $entries = @()
    foreach ($name in $names) {
        $path = Join-Path $directory $name
        New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
        [IO.File]::WriteAllText($path, "fixture $name")
        $entries += @{ path = $name; length = (Get-Item -LiteralPath $path).Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
    }
    $manifest = @{ schemaVersion = 2; version = 'test'; platform = 'windows-x64'; runtime = 'Cli'; sourceRevision = ('a' * 40); files = $entries }
    [IO.File]::WriteAllText((Join-Path $directory 'release.json'), ($manifest | ConvertTo-Json -Depth 5))
    $null = Test-ReleasePackage $directory
    foreach ($bad in @('../outside', 'scripts/../../outside', 'C:\Windows\file', '\\server\file', 'omdrop-cli.exe:stream', 'scripts//file',
        'omdrop-cli.exe.', 'omdrop-cli.exe ', 'NUL', 'con.txt', 'scripts/LPT1.log', 'star*.dll')) {
        Expect-Failure { Resolve-PackageFile $directory $bad }
    }
    [IO.File]::AppendAllText((Join-Path $directory 'omdrop-cli.exe'), 'tampered')
    Expect-Failure { Test-ReleasePackage $directory }
    [IO.File]::WriteAllText((Join-Path $directory 'omdrop-cli.exe'), 'fixture omdrop-cli.exe')
    [IO.File]::WriteAllText((Join-Path $directory 'extra.exe'), 'unlisted')
    Expect-Failure { Test-ReleasePackage $directory }
    Remove-Item -LiteralPath (Join-Path $directory 'extra.exe')
    $manifest.files = @($entries | Where-Object path -ne 'vcruntime140.dll')
    [IO.File]::WriteAllText((Join-Path $directory 'release.json'), ($manifest | ConvertTo-Json -Depth 5))
    Expect-Failure { Test-ReleasePackage $directory }
    $manifest.files = @($entries) + @($entries[0])
    [IO.File]::WriteAllText((Join-Path $directory 'release.json'), ($manifest | ConvertTo-Json -Depth 5))
    Expect-Failure { Test-ReleasePackage $directory }
    $manifest.files = $entries
    [IO.File]::WriteAllText((Join-Path $directory 'release.json'), ($manifest | ConvertTo-Json -Depth 5))
    $junction = Join-Path $directory 'linked-directory'
    New-Item -ItemType Junction -Path $junction -Target (Join-Path $directory 'scripts') | Out-Null
    try { Expect-Failure { Test-ReleasePackage $directory } } finally { [IO.Directory]::Delete($junction) }
    Expect-Failure { Assert-InstallChild $directory (Split-Path -Parent $directory) }
    $cliDirectory = Join-Path $directory 'cli-toolkit'
    New-Item -ItemType Directory -Path $cliDirectory | Out-Null
    $cliNames = @('omdrop-cli.exe', 'LICENSE', 'QUICKSTART.zh-CN.md', 'licenses/dependencies.json',
        'vcruntime140.dll', 'msvcp140.dll', 'licenses/visual-cpp-Redist.txt')
    $cliEntries = @()
    foreach ($name in $cliNames) {
        $path = Join-Path $cliDirectory $name
        New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
        [IO.File]::WriteAllText($path, "fixture $name")
        $cliEntries += @{ path = $name; length = (Get-Item -LiteralPath $path).Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
    }
    $manifest.runtime = 'Cli'
    $manifest.files = $cliEntries
    [IO.File]::WriteAllText((Join-Path $cliDirectory 'release.json'), ($manifest | ConvertTo-Json -Depth 5))
    $null = Test-ReleasePackage $cliDirectory
    $firmwareDirectory = Join-Path $directory 'firmware-fixture'
    New-Item -ItemType Directory -Path $firmwareDirectory | Out-Null
    $boot = New-Object byte[] 4096
    $boot[0] = 0xe9
    $boot[12] = 9
    $table = New-Object byte[] 4096
    $table[0] = 0xaa; $table[1] = 0x50; $table[2] = 1; $table[3] = 2
    [BitConverter]::GetBytes([uint32]0x9000).CopyTo($table, 4)
    [BitConverter]::GetBytes([uint32]0x6000).CopyTo($table, 8)
    $table[32] = 0xaa; $table[33] = 0x50
    [BitConverter]::GetBytes([uint32]0x10000).CopyTo($table, 36)
    [BitConverter]::GetBytes([uint32]0x100000).CopyTo($table, 40)
    [IO.File]::WriteAllBytes((Join-Path $firmwareDirectory 'bootloader.bin'), $boot)
    [IO.File]::WriteAllBytes((Join-Path $firmwareDirectory 'application.bin'), $boot)
    [IO.File]::WriteAllBytes((Join-Path $firmwareDirectory 'partition-table.bin'), $table)
    $imageEntries = @()
    foreach ($kind in @('bootloader', 'partition-table', 'application')) {
        $path = Join-Path $firmwareDirectory "$kind.bin"
        $offset = switch ($kind) { 'bootloader' { 0 } 'partition-table' { 32768 } 'application' { 65536 } }
        $imageEntries += @{ kind = $kind; path = "$kind.bin"; offset = $offset; length = 4096; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
    }
    $firmwareManifest = @{ formatVersion = 1; board = 'cardputer-adv'; chip = 'esp32s3'; flashSize = 8388608;
        firmwareVersion = 'fixture'; sourceRevision = ('a' * 40); protocolVersion = 2; idfVersion = '6.0';
        toolVersion = '5.0.2'; layoutId = $imageEntries[1].sha256; images = $imageEntries }
    $firmwareManifestPath = Join-Path $firmwareDirectory 'manifest.json'
    [IO.File]::WriteAllText($firmwareManifestPath, ($firmwareManifest | ConvertTo-Json -Depth 6))
    $null = Test-FirmwarePackage $firmwareDirectory
    foreach ($s3HeadlessProfile in @('esp32s3-uart0', 'esp32s3-native-usb')) {
        $firmwareManifest.board = $s3HeadlessProfile
        [IO.File]::WriteAllText($firmwareManifestPath, ($firmwareManifest | ConvertTo-Json -Depth 6))
        $null = Test-FirmwarePackage $firmwareDirectory
    }
    $firmwareManifest.board = 'cardputer-adv'
    [IO.File]::WriteAllText($firmwareManifestPath, ($firmwareManifest | ConvertTo-Json -Depth 6))
    $boot[12] = 0
    [IO.File]::WriteAllBytes((Join-Path $firmwareDirectory 'application.bin'), $boot)
    Expect-Failure { Test-FirmwarePackage $firmwareDirectory }
    $imageEntries[2].sha256 = (Get-FileHash -LiteralPath (Join-Path $firmwareDirectory 'application.bin') -Algorithm SHA256).Hash
    [IO.File]::WriteAllText($firmwareManifestPath, ($firmwareManifest | ConvertTo-Json -Depth 6))
    Expect-Failure { Test-FirmwarePackage $firmwareDirectory }
    $boot[12] = 9
    [IO.File]::WriteAllBytes((Join-Path $firmwareDirectory 'application.bin'), $boot)
    $imageEntries[2].sha256 = (Get-FileHash -LiteralPath (Join-Path $firmwareDirectory 'application.bin') -Algorithm SHA256).Hash
    [BitConverter]::GetBytes([uint32]0x10000).CopyTo($table, 4)
    [IO.File]::WriteAllBytes((Join-Path $firmwareDirectory 'partition-table.bin'), $table)
    $imageEntries[1].sha256 = (Get-FileHash -LiteralPath (Join-Path $firmwareDirectory 'partition-table.bin') -Algorithm SHA256).Hash
    $firmwareManifest.layoutId = $imageEntries[1].sha256
    [IO.File]::WriteAllText($firmwareManifestPath, ($firmwareManifest | ConvertTo-Json -Depth 6))
    Expect-Failure { Test-FirmwarePackage $firmwareDirectory }

    $c5Directory = Join-Path $directory 'c5-firmware-fixture'
    New-Item -ItemType Directory -Path $c5Directory | Out-Null
    $c5Boot = New-Object byte[] 4096
    $c5Boot[0] = 0xe9; $c5Boot[12] = 0x17
    $c5Application = New-Object byte[] 4096
    $c5Application[0] = 0xe9; $c5Application[12] = 0x17
    $c5Table = New-Object byte[] 3072
    for ($index = 0; $index -lt $c5Table.Length; $index++) { $c5Table[$index] = 0xff }
    foreach ($partition in @(
        @{ slot=0; type=1; subtype=2; offset=0x9000; size=0x6000; name='nvs' },
        @{ slot=1; type=1; subtype=1; offset=0xf000; size=0x1000; name='phy_init' },
        @{ slot=2; type=0; subtype=0; offset=0x10000; size=0x1f0000; name='factory' }
    )) {
        $start = $partition.slot * 32
        $c5Table[$start] = 0xaa; $c5Table[$start + 1] = 0x50
        $c5Table[$start + 2] = $partition.type; $c5Table[$start + 3] = $partition.subtype
        [BitConverter]::GetBytes([uint32]$partition.offset).CopyTo($c5Table, $start + 4)
        [BitConverter]::GetBytes([uint32]$partition.size).CopyTo($c5Table, $start + 8)
        [Text.Encoding]::ASCII.GetBytes($partition.name).CopyTo($c5Table, $start + 12)
    }
    [IO.File]::WriteAllBytes((Join-Path $c5Directory 'bootloader.bin'), $c5Boot)
    [IO.File]::WriteAllBytes((Join-Path $c5Directory 'partition-table.bin'), $c5Table)
    [IO.File]::WriteAllBytes((Join-Path $c5Directory 'application.bin'), $c5Application)
    $c5Images = @()
    foreach ($image in @(
        @{ kind='bootloader'; offset=8192; path='bootloader.bin' },
        @{ kind='partition-table'; offset=32768; path='partition-table.bin' },
        @{ kind='application'; offset=65536; path='application.bin' }
    )) {
        $path = Join-Path $c5Directory $image.path
        $c5Images += @{ kind=$image.kind; path=$image.path; offset=$image.offset; length=(Get-Item -LiteralPath $path).Length; sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
    }
    $c5Manifest = @{ formatVersion=1; board='esp32c5-uart0'; chip='esp32c5'; flashSize=2097152;
        firmwareVersion='fixture'; sourceRevision=('a' * 40); protocolVersion=2; idfVersion='6.0';
        toolVersion='5.0.2'; layoutId=$c5Images[1].sha256; images=$c5Images }
    $c5ManifestPath = Join-Path $c5Directory 'manifest.json'
    [IO.File]::WriteAllText($c5ManifestPath, ($c5Manifest | ConvertTo-Json -Depth 6))
    $null = Test-FirmwarePackage $c5Directory
    $c5Manifest.board = 'esp32c5-native-usb'
    [IO.File]::WriteAllText($c5ManifestPath, ($c5Manifest | ConvertTo-Json -Depth 6))
    $null = Test-FirmwarePackage $c5Directory
    $c5Application[12] = 9
    [IO.File]::WriteAllBytes((Join-Path $c5Directory 'application.bin'), $c5Application)
    $c5Manifest.board = 'esp32c5-uart0'
    $c5Images[2].sha256 = (Get-FileHash -LiteralPath (Join-Path $c5Directory 'application.bin') -Algorithm SHA256).Hash
    [IO.File]::WriteAllText($c5ManifestPath, ($c5Manifest | ConvertTo-Json -Depth 6))
    Expect-Failure { Test-FirmwarePackage $c5Directory }
    Write-Host 'Release validation passed: service/CLI packages, paths/junctions, hashes, required runtime, duplicates, firmware chip/layout and installation target boundaries.'
} finally {
    # Verify the absolute target remains this explicitly created temporary test directory.
    $expected = [IO.Path]::GetFullPath($directory)
    if ((Split-Path -Parent $expected) -ine ([IO.Path]::GetTempPath()).TrimEnd('\', '/') -or
        (Split-Path -Leaf $expected) -notlike 'omdrop-release-test-*') { throw 'Unsafe test cleanup path.' }
    Remove-Item -LiteralPath $expected -Recurse -Force
}
