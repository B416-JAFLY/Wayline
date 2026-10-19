# Build the native GPUI capsule. The release shader compiler comes from Windows SDK.
param([switch]$DebugBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$previousFxc = $env:GPUI_FXC_PATH
try {
    if (-not $DebugBuild -and (
        -not $env:GPUI_FXC_PATH -or
        -not (Test-Path -LiteralPath $env:GPUI_FXC_PATH -PathType Leaf -ErrorAction SilentlyContinue)
    )) {
        $sdk = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -ErrorAction SilentlyContinue
        if (-not $sdk.KitsRoot10) {
            $sdk = Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots' -ErrorAction SilentlyContinue
        }
        if (-not $sdk.KitsRoot10) { throw 'Install Windows 10/11 SDK (fxc.exe), or set GPUI_FXC_PATH to its full path.' }
        $compiler = Get-ChildItem -Path (Join-Path $sdk.KitsRoot10 'bin\*\x64\fxc.exe') -File -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1
        if (-not $compiler) { throw 'Install Windows 10/11 SDK (fxc.exe), or set GPUI_FXC_PATH to its full path.' }
        $env:GPUI_FXC_PATH = $compiler.FullName
    }
    $arguments = @('build', '--manifest-path', (Join-Path $root 'Cargo.toml'), '-p', 'omdrop-desktop')
    if (-not $DebugBuild) { $arguments += '--release' }
    & cargo @arguments
    if ($LASTEXITCODE) { throw 'Desktop build failed.' }
    $profile = if ($DebugBuild) { 'debug' } else { 'release' }
    Write-Host (Join-Path $root "target\$profile\omdrop-desktop.exe")
} finally {
    $env:GPUI_FXC_PATH = $previousFxc
}
