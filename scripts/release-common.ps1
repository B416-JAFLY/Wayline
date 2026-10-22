# Shared, read-only package validation. Compatible with Windows PowerShell 5.1.
Set-StrictMode -Version 2

function Get-ExecutableVersion([string]$Path) {
    # Windows-subsystem applications are not awaited by PowerShell's call operator.
    # Own their redirected pipes and lifetime explicitly; --version exits before UI setup.
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = [IO.Path]::GetFullPath($Path)
    $start.Arguments = '--version'
    $start.WorkingDirectory = Split-Path -Parent $start.FileName
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { throw "Cannot run version check: $Path" }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "Version check timed out: $Path"
        }
        $output = $stdout.GetAwaiter().GetResult()
        $diagnostic = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "Version check failed ($($process.ExitCode)): $Path $diagnostic" }
        return $output.Trim()
    } finally { $process.Dispose() }
}

function Resolve-PackageFile([string]$Root, [string]$RelativePath) {
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or
        [IO.Path]::IsPathRooted($RelativePath) -or $RelativePath.Contains(':') -or
        ($RelativePath -split '[/\\]' | Where-Object { $_ -eq '..' -or $_ -eq '.' -or $_ -eq '' })) {
        throw "Invalid package path: $RelativePath"
    }
    foreach ($part in ($RelativePath -split '[/\\]')) {
        if ($part -match '[<>"|?*\x00-\x1f]' -or $part -match '[. ]$' -or
            $part -match '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)') {
            throw "Invalid Windows package path: $RelativePath"
        }
    }
    $base = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $file = [IO.Path]::GetFullPath((Join-Path $base $RelativePath))
    if (-not $file.StartsWith($base + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Package path escapes its directory: $RelativePath"
    }
    $current = $file
    while ($current.Length -ge $base.Length) {
        $item = Get-Item -LiteralPath $current -Force -ErrorAction Stop
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Package links are not allowed: $current" }
        if ($current -eq $base) { break }
        $current = Split-Path -Parent $current
    }
    return $file
}

function Get-PackageFiles([string]$Root) {
    # Walk explicitly so that directory junctions are rejected before traversal.
    $pending = New-Object 'System.Collections.Generic.Stack[string]'
    $pending.Push([IO.Path]::GetFullPath($Root))
    while ($pending.Count) {
        $directory = $pending.Pop()
        if ((Get-Item -LiteralPath $directory -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Package links are not allowed: $directory"
        }
        foreach ($item in Get-ChildItem -LiteralPath $directory -Force) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Package links are not allowed: $($item.FullName)" }
            if ($item.PSIsContainer) { $pending.Push($item.FullName) } else { $item }
        }
    }
}

function Get-BridgeReleaseMetadata([string]$Root) {
    $build = Join-Path $Root 'target/release'
    $recordPath = Join-Path $build 'omdrop-bridge-build.json'
    $record = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($record.schemaVersion -ne 1 -or $record.profile -ne 'release' -or
        $record.upstreamRevision -ne '2e9b2b0228432d5e402a9c90a031b419960d0bd1' -or
        $record.sourceRevision -notmatch '^[0-9a-f]{40}$' -or @($record.sources).Count -eq 0) {
        throw 'Build the pinned Rust bridge with scripts/build-bridge.ps1 first.'
    }
    foreach ($entry in $record.sources) {
        $path = Resolve-PackageFile $Root $entry.path
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256) {
            throw "Bridge source changed after build: $($entry.path)"
        }
    }
    if ((Get-FileHash -LiteralPath (Join-Path $build 'omdrop-bridge.exe') -Algorithm SHA256).Hash -ine $record.executableSha256 -or
        (Get-FileHash -LiteralPath (Join-Path $build 'wintun.dll') -Algorithm SHA256).Hash -ine $record.wintunSha256 -or
        (Get-FileHash -LiteralPath (Join-Path $Root 'Cargo.lock') -Algorithm SHA256).Hash -ine $record.cargoLockSha256) {
        throw 'Bridge binaries or Cargo.lock changed after build.'
    }
    return [ordered]@{
        sourceRevision = $record.sourceRevision
        upstreamRevision = $record.upstreamRevision
        executableSha256 = $record.executableSha256
        wintunSha256 = $record.wintunSha256
        buildRecordSha256 = (Get-FileHash -LiteralPath $recordPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Get-AirDropReleaseMetadata([string]$Root) {
    $sources = @()
    foreach ($directory in @('crates/omdrop-core/src', 'vendor/owly-airdrop/src')) {
        $sources += @(Get-ChildItem -LiteralPath (Join-Path $Root $directory) -File -Recurse)
    }
    foreach ($file in @('Cargo.toml','Cargo.lock','crates/omdrop-core/Cargo.toml','vendor/owly-airdrop/Cargo.toml','vendor/owly-airdrop/LICENSE')) {
        $sources += Get-Item -LiteralPath (Join-Path $Root $file)
    }
    $entries = @($sources | Sort-Object FullName | ForEach-Object {
        [ordered]@{ path = $_.FullName.Substring($Root.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    return [ordered]@{ upstreamRevision = '2e9b2b0228432d5e402a9c90a031b419960d0bd1'; sources = $entries }
}

function Test-FirmwarePackage([string]$Root) {
    $manifestPath = Resolve-PackageFile $Root 'manifest.json'
    if ((Get-Item -LiteralPath $manifestPath).Length -gt 65536) { throw 'Firmware manifest is too large.' }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.formatVersion -ne 1 -or $manifest.board -ne 'cardputer-adv' -or $manifest.chip -ne 'esp32s3' -or
        $manifest.flashSize -ne 8388608 -or $manifest.protocolVersion -ne 2 -or $manifest.idfVersion -ne '6.0' -or
        $manifest.toolVersion -ne '5.0.2' -or $manifest.sourceRevision -notmatch '^[0-9a-f]{40}$' -or
        [string]::IsNullOrWhiteSpace($manifest.firmwareVersion) -or $manifest.firmwareVersion.Length -gt 128 -or
        $manifest.firmwareVersion -match '[\x00-\x1f]' -or @($manifest.images).Count -ne 3) { throw 'Unsupported firmware package.' }
    $kinds = @{}
    $paths = @{}
    $previousEnd = 0L
    foreach ($entry in $manifest.images | Sort-Object offset) {
        $expectedOffset = switch ($entry.kind) { 'bootloader' { 0L } 'partition-table' { 32768L } 'application' { 65536L } default { throw 'Unknown firmware image kind.' } }
        $length = [long]$entry.length
        $offset = [long]$entry.offset
        if ($offset -ne $expectedOffset -or $kinds.ContainsKey($entry.kind) -or $paths.ContainsKey($entry.path) -or
            $length -le 0 -or $length -gt 8388608 -or $offset -lt $previousEnd -or $offset + $length -gt 8388608) { throw 'Invalid firmware image ranges.' }
        $path = Resolve-PackageFile $Root $entry.path
        if ((Get-Item -LiteralPath $path).Length -ne $length -or $entry.sha256 -notmatch '^[0-9a-fA-F]{64}$' -or
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256) { throw "Firmware checksum mismatch: $($entry.path)" }
        if ($entry.kind -ne 'partition-table') {
            $stream = [IO.File]::OpenRead($path)
            try {
                $header = New-Object byte[] 24
                if ($stream.Read($header, 0, 24) -ne 24 -or $header[0] -ne 0xe9 -or [BitConverter]::ToUInt16($header, 12) -ne 9) { throw 'Expected an ESP32-S3 firmware image.' }
            } finally { $stream.Dispose() }
        }
        $kinds[$entry.kind] = $entry
        $paths[$entry.path] = $path
        $previousEnd = [long]([Math]::Ceiling(($offset + $length) / 4096.0) * 4096)
    }
    $table = $kinds['partition-table']
    if ($table.length -gt 4096 -or $manifest.layoutId -ine $table.sha256) { throw 'Invalid firmware partition table identity.' }
    $bytes = [IO.File]::ReadAllBytes($paths[$table.path])
    $partitions = @()
    $appFound = $false
    $nvsFound = $false
    for ($index = 0; $index + 32 -le $bytes.Length; $index += 32) {
        if ([BitConverter]::ToUInt16($bytes, $index) -ne 0x50aa) { break }
        $offset = [long][BitConverter]::ToUInt32($bytes, $index + 4)
        $length = [long][BitConverter]::ToUInt32($bytes, $index + 8)
        if ($offset -lt 36864 -or $length -le 0 -or $offset + $length -gt 8388608 -or $offset % 4096 -or $length % 4096) { throw 'Invalid firmware partition range.' }
        $partitions += @{ offset = $offset; end = $offset + $length }
        if ($bytes[$index + 2] -eq 0 -and $offset -eq 65536 -and $kinds['application'].length -le $length) { $appFound = $true }
        if ($bytes[$index + 2] -eq 1 -and $bytes[$index + 3] -eq 2) { $nvsFound = $true }
        if ($bytes[$index + 2] -ne 0) {
            foreach ($entry in $manifest.images) {
                $imageEnd = [long]([Math]::Ceiling(($entry.offset + $entry.length) / 4096.0) * 4096)
                if ($imageEnd -gt $offset -and $entry.offset -lt $offset + $length) { throw 'Firmware would erase a persistent data partition.' }
            }
        }
    }
    $previousEnd = 0L
    foreach ($partition in $partitions | Sort-Object { $_.offset }) {
        if ($partition.offset -lt $previousEnd) { throw 'Overlapping firmware partitions.' }
        $previousEnd = $partition.end
    }
    if (-not $appFound -or -not $nvsFound) { throw 'Firmware partition table must contain a matching application and NVS.' }
    return $manifest
}

function Test-ReleasePackage([string]$Root) {
    $manifestPath = Resolve-PackageFile $Root 'release.json'
    if ((Get-Item -LiteralPath $manifestPath).Length -gt 1048576) { throw 'Release manifest is too large.' }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.schemaVersion -ne 2 -or $manifest.platform -ne 'windows-x64' -or
        $manifest.runtime -notin @('Desktop', 'Rust', 'Cli') -or -not $manifest.version -or
        $manifest.sourceRevision -notmatch '^[0-9a-f]{40}$' -or @($manifest.files).Count -eq 0) {
        throw 'Unsupported release manifest.'
    }
    $seen = @{}
    foreach ($entry in $manifest.files) {
        if ($entry.path.Contains('\') -or $entry.path -ieq 'release.json') { throw "Noncanonical package entry: $($entry.path)" }
        if ($seen.ContainsKey($entry.path)) { throw "Duplicate package entry: $($entry.path)" }
        $seen[$entry.path] = $true
        $path = Resolve-PackageFile $Root $entry.path
        $item = Get-Item -LiteralPath $path
        if ($item.PSIsContainer -or $item.Length -ne $entry.length -or $entry.sha256 -notmatch '^[0-9a-fA-F]{64}$' -or
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256) {
            throw "Package verification failed: $($entry.path)"
        }
    }
    $binary = switch ($manifest.runtime) { 'Desktop' { 'omdrop-desktop.exe' } 'Rust' { 'omdrop-service.exe' } 'Cli' { 'omdrop-cli.exe' } }
    $requiredFiles = @($binary, 'LICENSE', 'licenses/dependencies.json', 'QUICKSTART.zh-CN.md',
        'vcruntime140.dll', 'msvcp140.dll', 'licenses/visual-cpp-Redist.txt')
    if ($manifest.runtime -ne 'Cli') {
        $requiredFiles += @('wintun.dll', 'install.cmd', 'scripts/install-prebuilt.ps1', 'scripts/release-common.ps1', 'scripts/set-service-access.ps1')
        if ($manifest.schemaVersion -eq 2) {
            if ($manifest.radioBackend -ne 'rust') { throw 'Unsupported default radio backend.' }
            $requiredFiles += @('omdrop-bridge.exe', 'omdrop-bridge-build.json', 'licenses/owly-LICENSE.txt', 'licenses/wintun-LICENSE.txt')
        }
    }
    foreach ($required in $requiredFiles) {
        if (-not $seen.ContainsKey($required)) { throw "Missing release file: $required" }
    }
    if ($manifest.runtime -ne 'Cli' -and $manifest.schemaVersion -eq 2) {
        $recordPath = Resolve-PackageFile $Root 'omdrop-bridge-build.json'
        $record = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $bridge = $manifest.bridge
        if ($record.schemaVersion -ne 1 -or $record.profile -ne 'release' -or
            $record.sourceRevision -ne $manifest.sourceRevision -or $bridge.sourceRevision -ne $manifest.sourceRevision -or
            $record.upstreamRevision -ne '2e9b2b0228432d5e402a9c90a031b419960d0bd1' -or
            $record.upstreamRevision -ne $bridge.upstreamRevision -or
            $record.executableSha256 -ine $bridge.executableSha256 -or $record.wintunSha256 -ine $bridge.wintunSha256 -or
            (Get-FileHash -LiteralPath $recordPath -Algorithm SHA256).Hash -ine $bridge.buildRecordSha256 -or
            (Get-FileHash -LiteralPath (Join-Path $Root 'omdrop-bridge.exe') -Algorithm SHA256).Hash -ine $bridge.executableSha256 -or
            (Get-FileHash -LiteralPath (Join-Path $Root 'wintun.dll') -Algorithm SHA256).Hash -ine $bridge.wintunSha256) {
            throw 'Rust bridge provenance does not match the release.'
        }
    }
    if ($manifest.runtime -eq 'Desktop') {
        foreach ($required in @('firmware/manifest.json', 'tools/esptool/esptool.exe', 'QUICKSTART.zh-CN.md')) {
            if (-not $seen.ContainsKey($required)) { throw "Missing release file: $required" }
        }
        $firmware = Test-FirmwarePackage (Join-Path $Root 'firmware')
        if ($firmware.sourceRevision -ne $manifest.sourceRevision -or $firmware.firmwareVersion -ne $manifest.firmwareVersion -or
            $firmware.toolVersion -ne $manifest.toolVersion) { throw 'Firmware and release metadata do not match.' }
    }
    foreach ($file in Get-PackageFiles $Root) {
        $relative = $file.FullName.Substring([IO.Path]::GetFullPath($Root).TrimEnd('\', '/').Length + 1).Replace('\', '/')
        if ($relative -ne 'release.json' -and -not $seen.ContainsKey($relative)) { throw "Unlisted package file: $relative" }
    }
    return $manifest
}

function Assert-InstallBridgeConfiguration([string]$ConfigPath, [string]$PackageRoot, [string]$InstallRoot) {
    $configuredPath = $null
    if (Test-Path -LiteralPath $ConfigPath -PathType Leaf) {
        $config = Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($config.PSObject.Properties['bridgePath']) { $configuredPath = $config.bridgePath }
    }
    if ([string]::IsNullOrEmpty($configuredPath)) {
        if (-not (Test-Path -LiteralPath (Join-Path $PackageRoot 'omdrop-bridge.exe') -PathType Leaf)) {
            throw 'The Rust bridge is missing from the package.'
        }
        return
    }
    if (-not [IO.Path]::IsPathRooted($configuredPath)) { throw 'Configured bridge executable path must be absolute.' }
    $install = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $path = [IO.Path]::GetFullPath($configuredPath)
    if ($path.StartsWith($install, [StringComparison]::OrdinalIgnoreCase)) {
        $null = Resolve-PackageFile $PackageRoot $path.Substring($install.Length)
    } elseif (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Configured bridge executable does not exist: $path"
    }
}
function Assert-InstallChild([string]$Parent, [string]$Target) {
    $parentPath = [IO.Path]::GetFullPath($Parent).TrimEnd('\', '/')
    $targetPath = [IO.Path]::GetFullPath($Target).TrimEnd('\', '/')
    if ((Split-Path -Parent $targetPath) -ine $parentPath) { throw "Unsafe installation target: $targetPath" }
    if ((Get-Item -LiteralPath $parentPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Installation parent must not be a link: $parentPath"
    }
    if (Test-Path -LiteralPath $targetPath) {
        if ((Get-Item -LiteralPath $targetPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Installation directory must not be a link: $targetPath"
        }
    }
    return $targetPath
}
