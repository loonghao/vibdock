<#
.SYNOPSIS
Acceptance check for a built VibDock Windows release archive.

.DESCRIPTION
Independent of the packaging step: given a Windows release ZIP, extract it and
assert it contains the console CLI (vibdock.exe), the GUI launcher
(vibdock-launcher.exe) and README.md, then verify each executable's PE subsystem
(CLI console = 3, launcher GUI = 2). Throws on the first missing artifact or
wrong subsystem, so a launcher-less or miscompiled archive is rejected.

Use it to validate a produced/downloaded archive offline while GitHub Actions is
billing-blocked, and after a real release to confirm the published asset.

.PARAMETER Archive
Path to the Windows release ZIP.

.PARAMETER EvidenceDirectory
Where to extract for inspection. Defaults to a sibling 'package-acceptance-<guid>'
directory next to this script.
#>
param(
    [Parameter(Mandatory)][string]$Archive,
    [string]$EvidenceDirectory = (Join-Path $PSScriptRoot ('package-acceptance-' + [guid]::NewGuid()))
)
$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
    throw "Archive not found: $Archive"
}
New-Item -ItemType Directory -Path $EvidenceDirectory -Force | Out-Null
Expand-Archive -LiteralPath $Archive -DestinationPath $EvidenceDirectory -Force

foreach ($name in @('vibdock.exe', 'vibdock-launcher.exe', 'README.md')) {
    if (-not (Test-Path -LiteralPath (Join-Path $EvidenceDirectory $name) -PathType Leaf)) {
        throw "Windows release archive is missing $name; extracted evidence: $EvidenceDirectory"
    }
}

function Get-PeSubsystem([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path).Path)
    if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "Invalid DOS executable header: $Path"
    }
    $peOffset = [BitConverter]::ToInt32($bytes, 60)
    if ($peOffset -lt 64 -or $peOffset -gt $bytes.Length - 94) {
        throw "Invalid PE header offset: $Path"
    }
    if ([BitConverter]::ToUInt32($bytes, $peOffset) -ne 0x00004550) {
        throw "Invalid PE signature: $Path"
    }
    $optional = $peOffset + 24
    $magic = [BitConverter]::ToUInt16($bytes, $optional)
    if ($magic -ne 0x10B -and $magic -ne 0x20B) {
        throw ("Unsupported PE optional header magic 0x{0:X}: {1}" -f $magic, $Path)
    }
    return [BitConverter]::ToUInt16($bytes, $optional + 68)
}

$cli = Get-PeSubsystem (Join-Path $EvidenceDirectory 'vibdock.exe')
if ($cli -ne 3) { throw "CLI entry must use the console subsystem (3); got $cli" }
$launcher = Get-PeSubsystem (Join-Path $EvidenceDirectory 'vibdock-launcher.exe')
if ($launcher -ne 2) { throw "Launcher entry must use the GUI subsystem (2); got $launcher" }

Write-Output "PASS: Windows archive includes vibdock.exe (subsystem $cli), vibdock-launcher.exe (subsystem $launcher), and README.md. Evidence: $EvidenceDirectory"
