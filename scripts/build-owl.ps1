# Build owl.exe (native/owl): OWL at the commit in native/owl/pins, with its patches/,
# for an ESP32 radio and a Wintun awdl0. Needs git and MinGW GCC on PATH;
# discovers installed CMake/Ninja without changing PATH. Touches no radio and needs no admin.
#   scripts/build-owl.ps1            -> native/owl/build/owl.exe (+ wintun.dll)
#   scripts/build-owl.ps1 -Test      -> build and run the hardware-free regression tests
[CmdletBinding()]
param(
    [string]$CMakePath,
    [string]$NinjaPath,
    [switch]$Test
)
$ErrorActionPreference = 'Stop'

function Resolve-BuildTool {
    param([string]$Name, [string]$Override, [string[]]$Candidates, [string]$Hint)
    if ($Override) {
        if (-not (Test-Path -LiteralPath $Override -PathType Leaf)) {
            throw "$Name was not found at '$Override'. $Hint"
        }
        return (Get-Item -LiteralPath $Override).FullName
    }
    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Get-Item -LiteralPath $candidate).FullName
        }
    }
    throw "$Name was not found. $Hint"
}

# Resolve prerequisites before fetching or changing the cached source tree.
$gitPath = Resolve-BuildTool -Name git.exe -Hint 'Install Git for Windows and add it to PATH.'
$gccPath = Resolve-BuildTool -Name gcc.exe -Hint 'Install MinGW-w64 GCC and add its bin directory to PATH.'
$cmakeCandidates = @()
$ninjaCandidates = @()
foreach ($programFilesRoot in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
    if ($programFilesRoot) {
        $cmakeCandidates += Join-Path $programFilesRoot 'CMake\bin\cmake.exe'
    }
}
$idfToolsRoots = @(
    $env:IDF_TOOLS_PATH,
    'C:\Espressif\tools',
    (Join-Path $env:USERPROFILE '.espressif\tools')
) | Where-Object { $_ } | Select-Object -Unique
foreach ($toolsRoot in $idfToolsRoots) {
    $cmakeCandidates += @(Get-ChildItem -Path (Join-Path $toolsRoot 'cmake\*\bin\cmake.exe') -File -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -ExpandProperty FullName)
    $ninjaCandidates += @(Get-ChildItem -Path (Join-Path $toolsRoot 'ninja\*\ninja.exe') -File -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -ExpandProperty FullName)
}
if (${env:ProgramFiles(x86)}) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $installations = @(& $vswhere -products '*' -requires Microsoft.VisualStudio.Component.VC.CMake.Project -property installationPath)
        if ($LASTEXITCODE) { throw 'vswhere failed to locate Visual Studio build tools.' }
        foreach ($installation in $installations) {
            $cmakeCandidates += Join-Path $installation 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
            $ninjaCandidates += Join-Path $installation 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe'
        }
    }
}
$cmake = Resolve-BuildTool -Name cmake.exe -Override $CMakePath -Candidates $cmakeCandidates `
    -Hint 'Install CMake, enable the Visual Studio CMake component, or pass -CMakePath <cmake.exe>.'
$ninja = Resolve-BuildTool -Name ninja.exe -Override $NinjaPath -Candidates $ninjaCandidates `
    -Hint 'Install Ninja, enable the Visual Studio CMake component, or pass -NinjaPath <ninja.exe>.'
if ($Test) {
    $ctest = Join-Path (Split-Path -Parent $cmake) 'ctest.exe'
    if (-not (Test-Path -LiteralPath $ctest -PathType Leaf)) {
        throw "ctest.exe was not found beside '$cmake'. Install a complete CMake distribution to use -Test."
    }
}
Write-Host "CMake: $cmake"
Write-Host "Ninja: $ninja"
Write-Host "GCC:   $gccPath"

$Root = Split-Path -Parent $PSScriptRoot
$Up = Join-Path $Root '.cache/upstream'
$Owl = Join-Path $Up 'owl'
$OwlRepo = 'https://github.com/jedbillyb/owl.git'
$WintunUrl = 'https://www.wintun.net/builds/wintun-0.14.1.zip'
$WintunSha256 = '07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51'

$pins = @{}
Get-Content (Join-Path $Root 'native/owl/pins') | Where-Object { $_ -match '^\s*([A-Z_]+)=(\S+)' } |
    ForEach-Object { $pins[$Matches[1]] = $Matches[2] }
$commit = $pins['OWL_COMMIT']

function git_ { & $gitPath -C $Owl @args; if ($LASTEXITCODE) { throw "git $args failed" } }

New-Item -ItemType Directory -Force $Up | Out-Null
if (-not (Test-Path (Join-Path $Owl '.git'))) {
    & $gitPath clone -q --recurse-submodules $OwlRepo $Owl
    if ($LASTEXITCODE) { throw 'clone failed' }
}
# The tree is reset on every build: edits in .cache/upstream/owl belong in a patch.
& $gitPath -C $Owl fetch -q origin
if ($LASTEXITCODE) { throw 'fetching OWL failed' }
git_ checkout -q -- .
git_ -c advice.detachedHead=false checkout -q $commit
git_ submodule update -q --init --recursive
Get-ChildItem (Join-Path $Root 'native/owl/patches/owl-*.patch') | Sort-Object Name | ForEach-Object {
    & $gitPath -C $Owl apply $_.FullName
    if ($LASTEXITCODE) { throw "$($_.Name) did not apply to owl@$commit" }
}

$Wintun = Join-Path $Up 'wintun'
if (-not (Test-Path (Join-Path $Wintun 'include/wintun.h'))) {
    $dl = Join-Path $Up 'wintun-dl'
    New-Item -ItemType Directory -Force $dl | Out-Null
    $zip = Join-Path $dl 'wintun.zip'
    Invoke-WebRequest $WintunUrl -OutFile $zip
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne $WintunSha256) { throw 'wintun.zip checksum mismatch' }
    Expand-Archive $zip -DestinationPath $dl -Force
    Move-Item (Join-Path $dl 'wintun') $Wintun
}

$src = Join-Path $Root 'native/owl'
$build = Join-Path $src 'build'
$testConfiguration = @()
if ($Test) { $testConfiguration += '-DBUILD_TESTING=ON' }
& $cmake -S $src -B $build -G Ninja "-DCMAKE_C_COMPILER:FILEPATH=$gccPath" `
    "-DCMAKE_MAKE_PROGRAM:FILEPATH=$ninja" -DCMAKE_BUILD_TYPE=RelWithDebInfo @testConfiguration
if ($LASTEXITCODE) { throw 'cmake configure failed' }
& $cmake --build $build
if ($LASTEXITCODE) { throw 'build failed' }
if ($Test) {
    & $ctest --test-dir $build --output-on-failure
    if ($LASTEXITCODE) { throw 'OWL regression tests failed' }
}
Write-Host "built $build\owl.exe: owl@$($commit.Substring(0,7)) + $((Get-ChildItem (Join-Path $Root 'native/owl/patches/owl-*.patch')).Count) patches"
