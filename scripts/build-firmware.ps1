# Build ESP32 radio firmware in an isolated output directory. Does not flash or stop services.
[CmdletBinding()]
param(
    [string]$IdfProfile,
    [ValidateSet('esp32s3', 'esp32c5')]
    [string]$Target = 'esp32s3',
    [switch]$S3Headless,
    [switch]$S3NativeUsb,
    [switch]$C5NativeUsb,
    [switch]$RxAggregation
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if ($RxAggregation -and $Target -ne 'esp32s3') {
    throw '-RxAggregation is only supported by the audited ESP32-S3 build.'
}
if ($C5NativeUsb -and $Target -ne 'esp32c5') {
    throw '-C5NativeUsb can only be used with -Target esp32c5.'
}
if ($S3Headless -and $Target -ne 'esp32s3') {
    throw '-S3Headless can only be used with -Target esp32s3.'
}
if ($S3NativeUsb -and -not $S3Headless) {
    throw '-S3NativeUsb requires -S3Headless.'
}
if ($S3NativeUsb -and $Target -ne 'esp32s3') {
    throw '-S3NativeUsb can only be used with -Target esp32s3.'
}

if ($IdfProfile) {
    . $IdfProfile
} elseif (-not $env:IDF_PATH -or -not $env:IDF_PYTHON_ENV_PATH) {
    $candidates = @(
        $(if ($env:IDF_TOOLS_PATH) { Join-Path $env:IDF_TOOLS_PATH 'eim_idf.json' }),
        'C:\Espressif\tools\eim_idf.json',
        (Join-Path $env:USERPROFILE '.espressif\eim_idf.json')
    )
    $installation = $null
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            $installation = (Get-Content -LiteralPath $candidate -Raw | ConvertFrom-Json).idfInstalled |
                Where-Object { $_.name -eq 'v6.0' -and (Test-Path -LiteralPath $_.activationScript) } |
                Select-Object -First 1
            if ($installation) { break }
        }
    }
    if (-not $installation) {
        throw 'Activate ESP-IDF 6.0 first, or pass -IdfProfile <ESP-IDF PowerShell activation script>.'
    }
    . $installation.activationScript
}

$python = Join-Path $env:IDF_PYTHON_ENV_PATH 'Scripts\python.exe'
$idf = Join-Path $env:IDF_PATH 'tools\idf.py'
if (-not (Test-Path -LiteralPath $python) -or -not (Test-Path -LiteralPath $idf)) {
    throw 'ESP-IDF Python environment is incomplete.'
}
$rustTarget = if ($Target -eq 'esp32c5') { 'riscv32imac-esp-espidf' } else { 'xtensa-esp32s3-espidf' }
$rustToolchain = 'esp'
& rustc "+$rustToolchain" --version
if ($LASTEXITCODE) { throw "Install the $rustToolchain ESP Rust toolchain first with espup." }
$availableTargets = & rustc "+$rustToolchain" --print target-list
if ($LASTEXITCODE -or -not ($availableTargets -contains $rustTarget)) {
    throw "The $rustToolchain Rust toolchain does not provide $rustTarget. Install the target with espup before building."
}

$firmware = Join-Path $root 'firmware'
$outputName = if ($Target -eq 'esp32c5') {
    $name = 'build-rust-esp32c5'
    if ($C5NativeUsb) { $name += '-usb' }
    $name
} elseif ($S3Headless) {
    if ($S3NativeUsb) { 'build-rust-esp32s3-headless-usb' } else { 'build-rust-esp32s3-headless' }
} else {
    'build-rust'
}
if ($RxAggregation) { $outputName = 'build-rust-aggregation' }
$output = Join-Path $firmware $outputName
$config = Join-Path $output 'sdkconfig'
$defaults = if ($Target -eq 'esp32c5') {
    'sdkconfig.defaults;sdkconfig.defaults.esp32c5'
} elseif ($S3Headless) {
    if ($S3NativeUsb) { 'sdkconfig.defaults;sdkconfig.defaults.esp32s3-headless-usb' }
    else { 'sdkconfig.defaults;sdkconfig.defaults.esp32s3-headless' }
} else {
    'sdkconfig.defaults;sdkconfig.defaults.cardputer'
}
if ($C5NativeUsb) { $defaults += ';sdkconfig.defaults.esp32c5-usb' }
if ($RxAggregation) { $defaults += ';sdkconfig.defaults.aggregation' }
$previousComponentManager = $env:IDF_COMPONENT_MANAGER
try {
    $extraArguments = @()
    if ($Target -eq 'esp32c5' -or $S3Headless) {
        # Headless targets do not use M5Unified; preserve the checked-in lockfile.
        $env:IDF_COMPONENT_MANAGER = '0'
        $extraArguments = @('-D', 'EXCLUDE_COMPONENTS=m5stack__m5gfx;m5stack__m5unified')
    }
    & $python $idf -C $firmware -B $output -D IDF_TARGET=$Target `
        -D "SDKCONFIG=$config" -D "SDKCONFIG_DEFAULTS=$defaults" @extraArguments build
    $buildExitCode = $LASTEXITCODE
} finally {
    if ($null -eq $previousComponentManager) {
        Remove-Item Env:IDF_COMPONENT_MANAGER -ErrorAction SilentlyContinue
    } else {
        $env:IDF_COMPONENT_MANAGER = $previousComponentManager
    }
}
if ($buildExitCode) { throw "$Target Rust firmware build failed." }
$profile = if ($Target -eq 'esp32s3' -and -not $S3Headless) { 'cardputer-adv' }
    elseif ($S3NativeUsb) { 'esp32s3-native-usb' }
    elseif ($S3Headless) { 'esp32s3-uart0' }
    elseif ($C5NativeUsb) { 'esp32c5-native-usb' }
    else { 'esp32c5-uart0' }
& (Join-Path $PSScriptRoot 'record-firmware-build.ps1') -BuildDirectory $output -Target $Target -Profile $profile
Write-Host "Firmware: $(Join-Path $output 'owl-radio.bin')"
