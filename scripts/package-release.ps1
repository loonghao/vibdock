<#
.SYNOPSIS
Package a VibDock release archive and its SHA256 sidecar.

.DESCRIPTION
Single source of truth for release packaging, shared by the GitHub Release
workflow on all three targets and runnable offline for fixture acceptance when
hosted Actions are unavailable.

On a Windows target the archive contains BOTH entry points - the console CLI
(vibdock.exe) and the GUI launcher (vibdock-launcher.exe) - plus README.md, and
packaging fails if any is missing. Each Windows executable is verified against
its PE subsystem: the CLI must be the console subsystem (3) and the launcher the
GUI subsystem (2), so a miscompiled launcher can never ship as a flashing
console window and the CLI can never ship without a console.

On Linux/macOS only the supported CLI entry is packaged. The launcher is
Windows-only and is never shipped there even when the build produced it.

The SHA256 sidecar always ends with a single LF so `sha256sum -c` accepts it on
every platform (Set-Content would emit CRLF on the Windows runner).

Tag, Target and the executable names are treated as untrusted input: each must
be a single file-name segment. They are validated before any path is built, and
every derived path is contained within the output directory before anything is
written or removed, so a crafted Tag/Target can never stage, archive, write or
recursively delete outside the output directory.

.PARAMETER Tag
Release tag, e.g. v0.2.0. A single file-name segment.

.PARAMETER Target
Rust target triple. A triple containing "windows" selects dual-entry Windows
packaging; any other triple selects CLI-only Unix packaging. A single file-name
segment.

.PARAMETER CliExecutable
File name of the console CLI in target/<Target>/release, e.g. vibdock.exe. A
single file-name segment.

.PARAMETER LauncherExecutable
File name of the GUI launcher in target/<Target>/release. Required on Windows
targets; ignored on Unix targets. A single file-name segment.

.PARAMETER OutputDirectory
Repository root that holds README.md and target/<Target>/release. Defaults to
the current directory. All outputs and staging stay within this directory.
#>
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][string]$Target,
    [Parameter(Mandatory)][string]$CliExecutable,
    [string]$LauncherExecutable,
    [string]$OutputDirectory = '.'
)

$ErrorActionPreference = 'Stop'

function Assert-SafeSegment([string]$Value, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw "$Label must not be empty"
    }
    if ($Value.IndexOfAny([char[]]@([char]'/', [char]'\', [char]0)) -ge 0) {
        throw "$Label '$Value' must be a single file-name segment and must not contain path separators"
    }
    if ($Value -eq '.' -or $Value -eq '..' -or $Value.Contains('..')) {
        throw "$Label '$Value' must not contain a parent-directory reference"
    }
    if ($Value.IndexOfAny([IO.Path]::GetInvalidFileNameChars()) -ge 0) {
        throw "$Label '$Value' contains characters that are invalid in a file name"
    }
    if ($Value.EndsWith('.') -or $Value.EndsWith(' ')) {
        throw "$Label '$Value' must not end with a dot or a space"
    }
}

function Test-Contained([string]$Root, [string]$Candidate) {
    $rootFull = [IO.Path]::GetFullPath($Root)
    $candFull = [IO.Path]::GetFullPath($Candidate)
    if ([string]::Equals($rootFull, $candFull, [StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    $prefix = $rootFull
    if (-not ($prefix.EndsWith([string][IO.Path]::DirectorySeparatorChar) -or $prefix.EndsWith([string][IO.Path]::AltDirectorySeparatorChar))) {
        $prefix += [IO.Path]::DirectorySeparatorChar
    }
    return $candFull.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)
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
    # Subsystem is at optional-header offset 68 for both PE32 and PE32+.
    return [BitConverter]::ToUInt16($bytes, $optional + 68)
}

# Validate every caller-controlled name BEFORE it is used to build any path,
# write any file, or remove anything.
Assert-SafeSegment $Tag 'Tag'
Assert-SafeSegment $Target 'Target'
Assert-SafeSegment $CliExecutable 'CliExecutable'

# Packaging behavior is driven by the target platform, not the host OS, so the
# offline harness can exercise the Unix path from any machine.
$isWindowsTarget = $Target -match 'windows'

if ($isWindowsTarget) {
    if ([string]::IsNullOrWhiteSpace($LauncherExecutable)) {
        throw "Windows packaging requires a launcher executable name"
    }
    Assert-SafeSegment $LauncherExecutable 'LauncherExecutable'
} elseif (-not [string]::IsNullOrWhiteSpace($LauncherExecutable)) {
    # The launcher is ignored on Unix, but if supplied it must still be safe.
    Assert-SafeSegment $LauncherExecutable 'LauncherExecutable'
}

if (-not (Test-Path -LiteralPath $OutputDirectory -PathType Container)) {
    throw "OutputDirectory does not exist or is not a directory: $OutputDirectory"
}
$outputRoot = (Resolve-Path -LiteralPath $OutputDirectory).Path

$package = "vibdock-$Tag-$Target"
# Stage into a fresh, unique sub-directory: nothing that already exists is ever
# removed, so no caller-derived path is recursively deleted.
$stageName = "vibdock-pkgstage-$([guid]::NewGuid().ToString('N'))"
$packagePath = Join-Path $outputRoot $stageName
$archive = if ($isWindowsTarget) { Join-Path $outputRoot "$package.zip" } else { Join-Path $outputRoot "$package.tar.gz" }

# Defense in depth: every derived path must live inside the output directory
# before any write or removal.
foreach ($p in @($packagePath, $archive)) {
    if (-not (Test-Contained $outputRoot $p)) {
        throw "Refusing to operate outside the output directory: $p"
    }
}

New-Item -ItemType Directory -Path $packagePath | Out-Null
try {
    $releaseDir = Join-Path $outputRoot "target/$Target/release"

    # Build the exact artifact set. Unix ships only the CLI; the launcher is
    # Windows-only and must never be packaged for Linux/macOS.
    $artifacts = [System.Collections.Generic.List[string]]::new()
    $artifacts.Add($CliExecutable)
    if ($isWindowsTarget) {
        $artifacts.Add($LauncherExecutable)
    }

    # Fail loudly on any missing artifact rather than shipping a partial archive.
    foreach ($artifact in $artifacts) {
        $source = Join-Path $releaseDir $artifact
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "Required release artifact is missing: $source"
        }
        Copy-Item -LiteralPath $source -Destination $packagePath
    }

    $readme = Join-Path $outputRoot 'README.md'
    if (-not (Test-Path -LiteralPath $readme -PathType Leaf)) {
        throw "Required release artifact is missing: $readme"
    }
    Copy-Item -LiteralPath $readme -Destination $packagePath

    # Console CLI must be subsystem 3; GUI launcher must be subsystem 2.
    if ($isWindowsTarget) {
        $cliSubsystem = Get-PeSubsystem (Join-Path $packagePath $CliExecutable)
        if ($cliSubsystem -ne 3) {
            throw "CLI entry '$CliExecutable' must use the console subsystem (3); got $cliSubsystem"
        }
        $launcherSubsystem = Get-PeSubsystem (Join-Path $packagePath $LauncherExecutable)
        if ($launcherSubsystem -ne 2) {
            throw "Launcher entry '$LauncherExecutable' must use the GUI subsystem (2); got $launcherSubsystem"
        }
    }

    if ($isWindowsTarget) {
        Compress-Archive -Path (Join-Path $packagePath '*') -DestinationPath $archive -Force
    } else {
        & tar -czf $archive -C $packagePath .
        if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
    }

    $archiveName = Split-Path -Leaf $archive
    $digest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    # GNU text-mode format "<digest><two spaces><file><LF>"; single LF terminator
    # keeps `sha256sum -c` working on every platform.
    "$digest  $archiveName`n" | Set-Content -LiteralPath "$archive.sha256" -Encoding ascii -NoNewline

    Write-Output "Packaged $archiveName (entries: $($artifacts -join ', '), README.md)"
}
finally {
    # Remove only the unique staging directory created above, and only after
    # confirming it is contained within the output directory.
    if (Test-Path -LiteralPath $packagePath) {
        if (-not (Test-Contained $outputRoot $packagePath)) {
            throw "Refusing to remove a staging path outside the output directory: $packagePath"
        }
        Remove-Item -Recurse -Force -LiteralPath $packagePath
    }
}
