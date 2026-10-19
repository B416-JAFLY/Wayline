# Developer-only download. Installation and flashing use the resulting offline files.
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$version = '5.0.2'
$sha256 = '1caef993a16c5915714a0da772d93f2e3239f316c06223981b262b838287268c'
$url = "https://github.com/espressif/esptool/releases/download/v$version/esptool-v$version-windows-amd64.zip"
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Output directory already exists; choose a new directory.' }
$download = Join-Path ([IO.Path]::GetTempPath()) ("omdrop-esptool-" + [Guid]::NewGuid().ToString('N') + '.zip')
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Invoke-WebRequest -Uri $url -OutFile $download -UseBasicParsing
if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash -ine $sha256) { throw 'esptool archive checksum mismatch.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
Expand-Archive -LiteralPath $download -DestinationPath $OutputDirectory
$executables = @(Get-ChildItem -LiteralPath $OutputDirectory -Filter esptool.exe -Recurse -File)
if ($executables.Count -ne 1) { throw 'Expected exactly one esptool.exe in the verified archive.' }
# Retain the upstream bundle layout and point callers to its executable directory.
$toolDirectory = $executables[0].Directory.FullName
Invoke-WebRequest -Uri "https://raw.githubusercontent.com/espressif/esptool/v$version/LICENSE" -OutFile (Join-Path $toolDirectory 'LICENSE.esptool') -UseBasicParsing
Invoke-WebRequest -Uri "https://github.com/espressif/esptool/archive/refs/tags/v$version.zip" -OutFile (Join-Path $toolDirectory 'esptool-source.zip') -UseBasicParsing
if ((Get-FileHash -LiteralPath (Join-Path $toolDirectory 'LICENSE.esptool') -Algorithm SHA256).Hash -ine '8177f97513213526df2cf6184d8ff986c675afb514d4e68a404010521b880643' -or
    (Get-FileHash -LiteralPath (Join-Path $toolDirectory 'esptool-source.zip') -Algorithm SHA256).Hash -ine 'df2ce32a46c091fb2f1358bb37a2ea5144253862d82d0c9872df07ccb9e7e436') {
    throw 'esptool license/source checksum mismatch.'
}
$record = [ordered]@{ version = $version; archiveUrl = $url; archiveSha256 = $sha256; executableSha256 = (Get-FileHash -LiteralPath $executables[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
[IO.File]::WriteAllText((Join-Path $toolDirectory 'tool.json'), ($record | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
Write-Host "Verified esptool directory: $toolDirectory"
