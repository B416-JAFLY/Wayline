# Load only the configuration helper and replace CIM calls. This test never touches SCM.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2
$installer = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts/install-prebuilt.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw "Installer parse errors: $parseErrors" }
$definition = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Set-PrebuiltServiceConfiguration'
}, $false)
if (-not $definition) { throw 'Missing service configuration helper.' }
. ([scriptblock]::Create($definition.Extent.Text))
$script:captured = $null
$script:returnCode = 0
$script:invokeFailure = $false
function Invoke-CimMethod {
    param($InputObject, $MethodName, $Arguments, $ErrorAction)
    if ($script:invokeFailure) { throw 'Simulated CIM transport error.' }
    $script:captured = @{ Service = $InputObject; Method = $MethodName; Arguments = $Arguments }
    [pscustomobject]@{ ReturnValue = $script:returnCode }
}
$service = [pscustomobject]@{ Name = 'OmdropOwl' }
$paths = @(
    '"C:\Program Files\omdrop-owl\omdrop-desktop.exe" --service --on-demand',
    '"D:\Apps With Spaces\old service.exe" --service --resident --config "D:\Old Config\service.json"'
)
foreach ($path in $paths) {
    foreach ($startup in @('Manual', 'Automatic', 'Disabled')) {
        Set-PrebuiltServiceConfiguration -Service $service -BinaryPath $path -Startup $startup
        if ($script:captured.Service -ne $service -or $script:captured.Method -ne 'Change' -or
            $script:captured.Arguments.PathName -cne $path -or $script:captured.Arguments.StartMode -cne $startup -or
            $script:captured.Arguments.Count -ne 2) { throw 'Service configuration arguments were altered.' }
    }
}
foreach ($code in @(2, 9, 21)) {
    $script:returnCode = $code
    $failure = $null
    try { Set-PrebuiltServiceConfiguration -Service $service -BinaryPath $paths[0] -Startup Manual } catch { $failure = $_ }
    if (-not $failure -or $failure.ToString() -notlike "*result: $code)*") { throw "CIM failure $code was not reported." }
}
$script:invokeFailure = $true
$failure = $null
try { Set-PrebuiltServiceConfiguration -Service $service -BinaryPath $paths[0] -Startup Manual } catch { $failure = $_ }
if (-not $failure -or $failure.ToString() -notlike '*Simulated CIM transport error*') { throw 'CIM transport failure was swallowed.' }
Write-Host 'Installer service configuration passed: quoted paths, rollback arguments, startup modes and CIM failures (SCM mocked).'
