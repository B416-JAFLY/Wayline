# Install verified files only. No compiler, downloads, radio access or firmware writes.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PackageRoot,
    [switch]$Elevated,
    [switch]$VerifyOnly
)
$ErrorActionPreference = 'Stop'
function Set-PrebuiltServiceConfiguration {
    param(
        [Parameter(Mandatory = $true)]$Service,
        [Parameter(Mandatory = $true)][string]$BinaryPath,
        [Parameter(Mandatory = $true)][ValidateSet('Automatic', 'Manual', 'Disabled')][string]$Startup
    )
    # Pass the quoted executable path as a CIM string. Windows PowerShell 5.1
    # native argument conversion can remove embedded quotes passed to sc.exe.
    $result = Invoke-CimMethod -InputObject $Service -MethodName Change -Arguments @{
        PathName = $BinaryPath
        StartMode = $Startup
    } -ErrorAction Stop
    if ($null -eq $result -or $null -eq $result.ReturnValue -or $result.ReturnValue -ne 0) {
        throw "Service configuration failed for $($Service.Name) (Win32_Service.Change result: $($result.ReturnValue))."
    }
}
. (Join-Path $PSScriptRoot 'release-common.ps1')
$PackageRoot = (Resolve-Path -LiteralPath $PackageRoot).Path
$manifest = Test-ReleasePackage $PackageRoot
if ($VerifyOnly) { Write-Host "Verified Omdrop $($manifest.version) ($($manifest.runtime))."; return }
if ($manifest.runtime -eq 'Cli') { throw 'The CLI toolkit is portable. Extract it and use the already installed Omdrop service; it has no service installer.' }
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    # This parent remains unelevated and launches the UI only after installation.
    $arguments = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -PackageRoot "{1}" -Elevated' -f $PSCommandPath, $PackageRoot
    $child = Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList $arguments -Wait -PassThru -WindowStyle Hidden
    if ($child.ExitCode -ne 0) { throw 'Installation failed. The previous installation was retained where possible.' }
    if ($manifest.runtime -eq 'Desktop') {
        $programFiles = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
        Start-Process -FilePath (Join-Path $programFiles 'omdrop-owl\omdrop-desktop.exe') -WindowStyle Hidden
    }
    return
}

$installerLock = [Threading.Mutex]::new($false, 'Global\OmdropOfflineInstaller')
$ownsInstallerLock = $false
try {
try { $ownsInstallerLock = $installerLock.WaitOne(0) } catch [Threading.AbandonedMutexException] { $ownsInstallerLock = $true }
if (-not $ownsInstallerLock) { throw 'Another Omdrop installation is running.' }
$programFiles = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
$dest = Assert-InstallChild $programFiles (Join-Path $programFiles 'omdrop-owl')
$suffix = [Guid]::NewGuid().ToString('N')
$staging = Assert-InstallChild $programFiles (Join-Path $programFiles "omdrop-owl-staging-$suffix")
$backup = Assert-InstallChild $programFiles (Join-Path $programFiles "omdrop-owl-backup-$suffix")
$name = 'OmdropOwl'
$existing = Get-Service $name -ErrorAction SilentlyContinue
$oldConfig = Get-CimInstance Win32_Service -Filter "Name='OmdropOwl'"
$oldRunning = $existing -and $existing.Status -ne 'Stopped'
$oldSddl = $null
if ($existing) {
    $security = & sc.exe sdshow $name
    if ($LASTEXITCODE) { throw 'Cannot preserve the installed service permissions.' }
    $oldSddl = ($security | Where-Object { $_.Trim().StartsWith('D:') }) -join ''
    if (-not $oldSddl) { throw 'The installed service returned no security descriptor.' }
}
$servicePid = if ($oldConfig) { $oldConfig.ProcessId } else { 0 }
$openDesktop = Get-CimInstance Win32_Process -Filter "Name='omdrop-desktop.exe'" |
    Where-Object { $_.ProcessId -ne $servicePid }
if ($openDesktop) { throw 'Quit Omdrop from its tray menu before updating.' }
# Active CLI users also share the service. Do not terminate their transfers.
$openCli = Get-CimInstance Win32_Process -Filter "Name='omdrop-cli.exe'"
if ($openCli) { throw 'Finish and exit Omdrop CLI operations before updating.' }
$hadPrevious = Test-Path -LiteralPath $dest
$replaced = $false
$registered = $false
$serviceTouched = $false
try {
    New-Item -ItemType Directory -Path $staging | Out-Null
    foreach ($entry in $manifest.files) {
        $source = Resolve-PackageFile $PackageRoot $entry.path
        $target = Join-Path $staging $entry.path
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
        Copy-Item -LiteralPath $source -Destination $target
    }
    Copy-Item -LiteralPath (Join-Path $PackageRoot 'release.json') -Destination $staging
    $null = Test-ReleasePackage $staging
    $serviceTouched = $true
    if ($oldRunning) { Stop-Service $name; $existing.WaitForStatus('Stopped', '00:00:30') }
    if ($hadPrevious) {
        $null = Assert-InstallChild $programFiles $dest
        $null = Assert-InstallChild $programFiles $backup
        Move-Item -LiteralPath $dest -Destination $backup
    }
    $null = Assert-InstallChild $programFiles $staging
    $null = Assert-InstallChild $programFiles $dest
    Move-Item -LiteralPath $staging -Destination $dest
    $replaced = $true
    $binary = if ($manifest.runtime -eq 'Desktop') { 'omdrop-desktop.exe' } else { 'omdrop-service.exe' }
    $mode = if ($manifest.runtime -eq 'Desktop') { '--on-demand' } else { '--resident' }
    $binaryPath = '"{0}" --service {1}' -f (Join-Path $dest $binary), $mode
    $startup = if ($manifest.runtime -eq 'Desktop') { 'Manual' } else { 'Automatic' }
    if ($existing) {
        Set-PrebuiltServiceConfiguration -Service $oldConfig -BinaryPath $binaryPath -Startup $startup
    } else {
        New-Service -Name $name -BinaryPathName $binaryPath -DisplayName 'Omdrop AirDrop radio' -StartupType $startup | Out-Null
        $registered = $true
    }
    & (Join-Path $dest 'scripts/set-service-access.ps1')
    if ($registered) {
        & sc.exe failure $name reset= 86400 actions= restart/5000/restart/30000 | Out-Null
        if ($LASTEXITCODE) { throw 'Service recovery configuration failed.' }
        & sc.exe failureflag $name 1 | Out-Null
        if ($LASTEXITCODE) { throw 'Service recovery configuration failed.' }
    }
    if ($manifest.runtime -eq 'Rust') { Start-Service $name }
    # Keep the previous files as a recoverable backup; user data is stored elsewhere.
    Write-Host "Installed Omdrop $($manifest.version) in $dest"
    if ($hadPrevious) { Write-Host "Previous files retained in $backup" }
    if (-not $Elevated -and $manifest.runtime -eq 'Desktop') { Write-Host 'Open omdrop-desktop.exe from Explorer as a normal user.' }
} catch {
    $failure = $_
    try {
        $service = Get-Service $name -ErrorAction SilentlyContinue
        if ($serviceTouched -and $service -and $service.Status -ne 'Stopped') { Stop-Service $name; $service.WaitForStatus('Stopped', '00:00:30') }
        if ($replaced) {
            $failed = Assert-InstallChild $programFiles (Join-Path $programFiles "omdrop-owl-failed-$suffix")
            $null = Assert-InstallChild $programFiles $dest
            Move-Item -LiteralPath $dest -Destination $failed
        }
        if (Test-Path -LiteralPath $backup) {
            $null = Assert-InstallChild $programFiles $backup
            $null = Assert-InstallChild $programFiles $dest
            Move-Item -LiteralPath $backup -Destination $dest
        }
        if ($serviceTouched -and $oldConfig) {
            $oldStart = switch ($oldConfig.StartMode) { 'Auto' { 'Automatic' } 'Disabled' { 'Disabled' } default { 'Manual' } }
            Set-PrebuiltServiceConfiguration -Service $oldConfig -BinaryPath $oldConfig.PathName -Startup $oldStart
            & sc.exe sdset $name $oldSddl | Out-Null
            if ($LASTEXITCODE) { throw 'Failed to restore service permissions.' }
            if ($oldRunning) { Start-Service $name }
        } elseif ($registered) {
            & sc.exe delete $name | Out-Null
            if ($LASTEXITCODE) { throw 'Failed to remove incomplete service registration.' }
        }
    } catch { Write-Warning "Recovery requires attention: $_. Previous files: $backup" }
    throw $failure
}
} finally {
    if ($ownsInstallerLock) { $installerLock.ReleaseMutex() }
    $installerLock.Dispose()
}
