# Allow the desktop, running as an ordinary interactive user, to start/stop
# only OmdropOwl. Run once as administrator for an existing installation.
# SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP; no configuration rights.
# https://learn.microsoft.com/en-us/windows/win32/services/service-security-and-access-rights
$ErrorActionPreference = 'Stop'
$serviceName = 'OmdropOwl'
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'run this from an administrator terminal'
}

$output = & sc.exe sdshow $serviceName
if ($LASTEXITCODE) { throw 'cannot read the OmdropOwl service permissions' }
$sddl = ($output | Where-Object { $_.Trim().StartsWith('D:') }) -join ''
if (-not $sddl) { throw 'sc.exe returned no service security descriptor' }
$descriptor = [Security.AccessControl.RawSecurityDescriptor]::new($sddl)
$acl = $descriptor.DiscretionaryAcl
if ($null -eq $acl) { throw 'refusing to replace an absent service DACL' }
$interactive = [Security.Principal.SecurityIdentifier]::new('S-1-5-4')
$granted = 0
foreach ($ace in $acl) {
    if ($ace -is [Security.AccessControl.CommonAce] -and
        $ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessAllowed -and
        $ace.SecurityIdentifier -eq $interactive) {
        $granted = $granted -bor $ace.AccessMask
    }
}
$missing = 0x34 -band (-bnot $granted)
if ($missing) {
    $entry = [Security.AccessControl.CommonAce]::new(
        [Security.AccessControl.AceFlags]::None,
        [Security.AccessControl.AceQualifier]::AccessAllowed,
        $missing, $interactive, $false, $null)
    # Keep existing restrictions and put the explicit allow before inherited ACEs.
    $index = 0
    while ($index -lt $acl.Count -and -not $acl[$index].IsInherited) { $index++ }
    $acl.InsertAce($index, $entry)
    & sc.exe sdset $serviceName ($descriptor.GetSddlForm([Security.AccessControl.AccessControlSections]::Access)) | Out-Null
    if ($LASTEXITCODE) { throw 'cannot update the OmdropOwl service permissions' }
}
Write-Host 'OmdropOwl permits interactive users to query, start and stop the service.'
