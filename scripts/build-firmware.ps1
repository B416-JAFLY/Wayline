# Build Cardputer Adv firmware in an isolated output directory. Does not flash or stop services.
[CmdletBinding()]
param(
    [string]$IdfProfile,
    [switch]$RxAggregation
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

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
& rustc +esp --version
if ($LASTEXITCODE) { throw 'Install the Xtensa toolchain first: espup install --targets esp32s3 --std' }

$firmware = Join-Path $root 'firmware'
$output = Join-Path $firmware 'build-rust'
if ($RxAggregation) { $output = Join-Path $firmware 'build-rust-aggregation' }
$config = Join-Path $output 'sdkconfig'
$defaults = 'sdkconfig.defaults;sdkconfig.defaults.cardputer'
if ($RxAggregation) { $defaults += ';sdkconfig.defaults.aggregation' }
& $python $idf -C $firmware -B $output -D IDF_TARGET=esp32s3 `
    -D "SDKCONFIG=$config" -D "SDKCONFIG_DEFAULTS=$defaults" build
if ($LASTEXITCODE) { throw 'Cardputer Rust firmware build failed.' }
& (Join-Path $PSScriptRoot 'record-firmware-build.ps1') -BuildDirectory $output
Write-Host "Firmware: $(Join-Path $output 'owl-radio.bin')"
