# Install (or update) the OmdropOwl service: publishes it to
# %ProgramFiles%\omdrop-owl with omdrop-bridge.exe and wintun.dll beside it. The default
# desktop binary also runs the LocalSystem service with --service, on demand.
# Run as an administrator; launch the desktop normally afterwards.
#   scripts\install-service.ps1            install/update the combined desktop + service
#   scripts\install-service.ps1 -Runtime Rust  use the standalone Rust supervisor
#   scripts\install-service.ps1 -Uninstall stop and remove the service (files stay)
param(
    [switch]$Uninstall,
    [switch]$Build,
    [string]$PackageRoot,
    [ValidateSet('Desktop', 'Rust')][string]$Runtime = 'Desktop'
)
$ErrorActionPreference = 'Stop'
if ($PackageRoot) {
    if ($Uninstall -or $Build) { throw '-PackageRoot cannot be combined with -Build or -Uninstall.' }
    & (Join-Path $PSScriptRoot 'install-prebuilt.ps1') -PackageRoot $PackageRoot
    return
}
$Root = Split-Path -Parent $PSScriptRoot
# The 64-bit Program Files even from a 32-bit PowerShell, where $env:ProgramFiles is the x86 one.
$programFiles = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles } # no ?? in PowerShell 5.1
$Dest = Join-Path $programFiles 'omdrop-owl'
$Name = 'OmdropOwl'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'run this from an administrator terminal'
}

$existing = Get-Service $Name -ErrorAction SilentlyContinue
if ($Uninstall) {
    if ($existing) {
        if ($existing.Status -ne 'Stopped') { Stop-Service $Name; $existing.WaitForStatus('Stopped', '00:00:15') }
        sc.exe delete $Name | Out-Null
        if ($LASTEXITCODE) { throw 'service deletion failed' }
        Write-Host "removed the $Name service"
    }
    return
}

if ($Build) { & (Join-Path $PSScriptRoot 'build-bridge.ps1') }
$bridge = Join-Path $Root 'target\release\omdrop-bridge.exe'
$wintun = Join-Path $Root 'target\release\wintun.dll'
if (-not (Test-Path -LiteralPath $bridge)) { throw 'Build the Rust bridge first: scripts/build-bridge.ps1' }
if (-not (Test-Path -LiteralPath $wintun)) { throw 'Build the verified Wintun bundle first: scripts/build-bridge.ps1' }
. (Join-Path $PSScriptRoot 'release-common.ps1')
$null = Get-BridgeReleaseMetadata $Root

# Complete the build and check for an open installed UI before stopping the
# service. The desktop and its service use the same on-disk executable.
if ($Runtime -eq 'Desktop') {
    if ($Build) { & (Join-Path $PSScriptRoot 'build-desktop.ps1') }
    $sourceExe = Join-Path $Root 'target\release\omdrop-desktop.exe'
    $exe = Join-Path $Dest 'omdrop-desktop.exe'
    $binaryPath = "`"$exe`" --service --on-demand"
    $serviceProcess = Get-CimInstance Win32_Service -Filter "Name='OmdropOwl'"
    $servicePid = if ($serviceProcess) { $serviceProcess.ProcessId } else { 0 }
    $openDesktop = Get-CimInstance Win32_Process -Filter "Name='omdrop-desktop.exe'" |
        Where-Object { $_.ExecutablePath -ieq $exe -and $_.ProcessId -ne $servicePid }
    if ($openDesktop) { throw 'Quit the installed Omdrop desktop from its tray menu before updating.' }
} else {
    $target = Join-Path $Root 'target'
    if ($Build) {
        $cargo = Get-Command cargo -ErrorAction Stop
        & $cargo.Source build --manifest-path (Join-Path $Root 'Cargo.toml') --target-dir $target -p omdrop-service --release --locked
        if ($LASTEXITCODE) { throw 'Rust service build failed; installed service was not stopped' }
    }
    $sourceExe = Join-Path $target 'release\omdrop-service.exe'
    $exe = Join-Path $Dest 'omdrop-service.exe'
    $binaryPath = "`"$exe`" --service --resident"
}
if (-not (Test-Path -LiteralPath $sourceExe -PathType Leaf)) { throw 'Build the host binary first, or pass -Build.' }
Assert-InstallBridgeConfiguration -ConfigPath (Join-Path $env:ProgramData 'omdrop-owl/config.json') `
    -PackageRoot (Join-Path $Root 'target/release') -InstallRoot $Dest

if ($existing -and $existing.Status -ne 'Stopped') {
    Stop-Service $Name
    $existing.WaitForStatus('Stopped', '00:00:15')
}
New-Item -ItemType Directory -Force $Dest | Out-Null
Copy-Item -LiteralPath $sourceExe -Destination $exe -Force
Copy-Item -LiteralPath $bridge, $wintun, (Join-Path $Root 'target/release/omdrop-bridge-build.json') -Destination $Dest -Force
Copy-Item -LiteralPath (Join-Path $Root 'target/release/bridge-licenses') -Destination $Dest -Recurse -Force

# The service runs the Rust bridge; explicit bridgePath settings are preserved.

$startupType = if ($Runtime -eq 'Desktop') { 'Manual' } else { 'Automatic' }
if (-not $existing) {
    New-Service -Name $Name -BinaryPathName $binaryPath -DisplayName 'omdrop-owl (AirDrop radio)' `
        -Description 'Runs the Rust AWDL bridge over an ESP32 and Wintun awdl0.' -StartupType $startupType | Out-Null
} else {
    # An earlier install may have gone elsewhere (Program Files (x86), from a 32-bit shell).
    sc.exe config $Name binPath= $binaryPath | Out-Null
    if ($LASTEXITCODE) { throw 'service configuration failed' }
    Set-Service -Name $Name -StartupType $startupType
}
& (Join-Path $PSScriptRoot 'set-service-access.ps1')
sc.exe failure $Name reset= 86400 actions= restart/5000/restart/30000 | Out-Null
if ($LASTEXITCODE) { throw 'service recovery configuration failed' }
sc.exe failureflag $Name 1 | Out-Null
if ($LASTEXITCODE) { throw 'service recovery flag configuration failed' }
if ($Runtime -eq 'Desktop') {
    Write-Host "$Name installed on demand; open $exe normally to start Omdrop."
} else {
    Start-Service $Name
    Write-Host "$Name ($Runtime) installed in $Dest and started; log: $env:ProgramData\omdrop-owl\service.log"
}
