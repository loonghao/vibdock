<#
.SYNOPSIS
Offline acceptance tests for scripts/package-release.ps1.

.DESCRIPTION
Validates the release packaging contract without hosted Actions or real build
artifacts by generating minimal PE stub executables with controlled subsystem
values and ordinary files for the Unix path. Each case asserts either a
successfully packaged archive (contents, launcher inclusion/exclusion, LF
sidecar) or a specific failure (missing artifact, wrong subsystem, non-PE).

package-release.ps1 is run as a child pwsh process so its exit code and any
`throw`/`exit` are isolated from this harness. Exits 0 when every case passes,
1 otherwise. Suitable for local verification while GitHub Actions billing is
unavailable and for CI once it is restored.
#>
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'package-release.ps1'
$pwsh = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
if (-not $pwsh) { Write-Host 'pwsh is required but was not found on PATH'; exit 2 }
$root = Join-Path ([IO.Path]::GetTempPath()) ('vibdock-pkgtest-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $root | Out-Null

$passed = 0
$failed = 0

function New-PeStub([string]$Path, [int]$Subsystem) {
    $buf = New-Object byte[] 160
    $buf[0] = 0x4D; $buf[1] = 0x5A                       # MZ
    [BitConverter]::GetBytes([int]64).CopyTo($buf, 60)   # PE header offset
    $buf[64] = 0x50; $buf[65] = 0x45                     # 'PE\0\0'
    [BitConverter]::GetBytes([uint16]0x20B).CopyTo($buf, 88)   # PE32+ magic
    [BitConverter]::GetBytes([uint16]$Subsystem).CopyTo($buf, 156) # Subsystem
    [IO.File]::WriteAllBytes($Path, $buf)
}

function New-Case([string]$Name, [string]$Target) {
    $dir = Join-Path $root $Name
    $release = Join-Path $dir "target/$Target/release"
    New-Item -ItemType Directory -Path $release -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $dir 'README.md') -Value 'VibDock' -NoNewline
    return $dir
}

function Invoke-Package([string]$Dir, [string]$Target, [string]$Cli, [string]$Launcher) {
    $a = @('-NoProfile', '-File', $script, '-Tag', 'v9.9.9', '-Target', $Target,
           '-CliExecutable', $Cli, '-OutputDirectory', $Dir)
    if ($Launcher) { $a += @('-LauncherExecutable', $Launcher) }
    $out = & $pwsh @a 2>&1
    return @{ Code = $LASTEXITCODE; Output = ($out | Out-String) }
}

function Assert-Case([string]$Name, [scriptblock]$Body) {
    try {
        $result = & $Body
        if ($result.Success) {
            Write-Host "PASS  $Name"
            $script:passed++
        } else {
            Write-Host "FAIL  $Name -> $($result.Detail)"
            $script:failed++
        }
    } catch {
        Write-Host "FAIL  $Name -> unexpected exception: $($_.Exception.Message)"
        $script:failed++
    }
}

function Assert-FailsWith([hashtable]$Run, [string]$Pattern) {
    if ($Run.Code -eq 0) { return @{ Success = $false; Detail = 'expected failure, got success' } }
    if ($Run.Output -notmatch $Pattern) { return @{ Success = $false; Detail = "unexpected error: $($Run.Output.Trim())" } }
    return @{ Success = $true }
}

$winTarget = 'x86_64-pc-windows-msvc'
$unixTarget = 'x86_64-unknown-linux-gnu'

# 1. Windows complete: both entry points + README, correct subsystems, LF sidecar.
Assert-Case 'windows complete archive includes both entries + README' {
    $dir = New-Case 'win-complete' $winTarget
    $rel = Join-Path $dir "target/$winTarget/release"
    New-PeStub (Join-Path $rel 'vibdock.exe') 3
    New-PeStub (Join-Path $rel 'vibdock-launcher.exe') 2
    $run = Invoke-Package $dir $winTarget 'vibdock.exe' 'vibdock-launcher.exe'
    if ($run.Code -ne 0) { return @{ Success = $false; Detail = "package-release exited $($run.Code): $($run.Output.Trim())" } }
    $zip = Join-Path $dir 'vibdock-v9.9.9-x86_64-pc-windows-msvc.zip'
    if (-not (Test-Path -LiteralPath $zip)) { return @{ Success = $false; Detail = 'zip not produced' } }
    $extract = Join-Path $dir 'extract'
    Expand-Archive -LiteralPath $zip -DestinationPath $extract
    $entries = Get-ChildItem -LiteralPath $extract -Name | Sort-Object
    $expected = @('README.md', 'vibdock-launcher.exe', 'vibdock.exe') | Sort-Object
    if (($entries -join ',') -ne ($expected -join ',')) {
        return @{ Success = $false; Detail = "entries: $($entries -join ',')" }
    }
    $bytes = [IO.File]::ReadAllBytes("$zip.sha256")
    if ($bytes[-1] -ne 0x0A -or $bytes[-2] -eq 0x0D) { return @{ Success = $false; Detail = 'sidecar not LF-terminated' } }
    $text = [Text.Encoding]::ASCII.GetString($bytes)
    if ($text -notmatch '^[0-9a-f]{64}  vibdock-v9\.9\.9-x86_64-pc-windows-msvc\.zip\n$') {
        return @{ Success = $false; Detail = "sidecar format: $text" }
    }
    $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if (-not $text.StartsWith($actual)) { return @{ Success = $false; Detail = 'sidecar digest mismatch' } }
    return @{ Success = $true }
}

# 2. Windows missing launcher must fail.
Assert-Case 'windows missing launcher is rejected' {
    $dir = New-Case 'win-missing-launcher' $winTarget
    New-PeStub (Join-Path $dir "target/$winTarget/release/vibdock.exe") 3
    $run = Invoke-Package $dir $winTarget 'vibdock.exe' 'vibdock-launcher.exe'
    return Assert-FailsWith $run 'missing'
}

# 3. Windows launcher with console subsystem must fail (would flash a console).
Assert-Case 'windows launcher with console subsystem is rejected' {
    $dir = New-Case 'win-launcher-console' $winTarget
    $rel = Join-Path $dir "target/$winTarget/release"
    New-PeStub (Join-Path $rel 'vibdock.exe') 3
    New-PeStub (Join-Path $rel 'vibdock-launcher.exe') 3
    $run = Invoke-Package $dir $winTarget 'vibdock.exe' 'vibdock-launcher.exe'
    return Assert-FailsWith $run 'GUI subsystem'
}

# 4. Windows CLI with GUI subsystem must fail (would have no console).
Assert-Case 'windows CLI with GUI subsystem is rejected' {
    $dir = New-Case 'win-cli-gui' $winTarget
    $rel = Join-Path $dir "target/$winTarget/release"
    New-PeStub (Join-Path $rel 'vibdock.exe') 2
    New-PeStub (Join-Path $rel 'vibdock-launcher.exe') 2
    $run = Invoke-Package $dir $winTarget 'vibdock.exe' 'vibdock-launcher.exe'
    return Assert-FailsWith $run 'console subsystem'
}

# 5. Windows non-PE CLI must fail (defends the subsystem parse).
Assert-Case 'windows non-PE CLI is rejected' {
    $dir = New-Case 'win-cli-nonpe' $winTarget
    $rel = Join-Path $dir "target/$winTarget/release"
    Set-Content -LiteralPath (Join-Path $rel 'vibdock.exe') -Value 'not a pe'
    New-PeStub (Join-Path $rel 'vibdock-launcher.exe') 2
    $run = Invoke-Package $dir $winTarget 'vibdock.exe' 'vibdock-launcher.exe'
    return Assert-FailsWith $run 'DOS executable header'
}

# 6. Windows missing README must fail.
Assert-Case 'windows missing README is rejected' {
    $dir = New-Case 'win-missing-readme' $winTarget
    $rel = Join-Path $dir "target/$winTarget/release"
    New-PeStub (Join-Path $rel 'vibdock.exe') 3
    New-PeStub (Join-Path $rel 'vibdock-launcher.exe') 2
    Remove-Item -LiteralPath (Join-Path $dir 'README.md')
    $run = Invoke-Package $dir $winTarget 'vibdock.exe' 'vibdock-launcher.exe'
    return Assert-FailsWith $run 'README'
}

# 7. Unix packages the CLI only, even when a launcher is present in the tree.
Assert-Case 'unix archive includes CLI + README and excludes launcher' {
    $dir = New-Case 'unix-cli-only' $unixTarget
    $rel = Join-Path $dir "target/$unixTarget/release"
    Set-Content -LiteralPath (Join-Path $rel 'vibdock') -Value 'cli' -NoNewline
    Set-Content -LiteralPath (Join-Path $rel 'vibdock-launcher') -Value 'launcher-should-not-ship' -NoNewline
    $run = Invoke-Package $dir $unixTarget 'vibdock' $null
    if ($run.Code -ne 0) { return @{ Success = $false; Detail = "package-release exited $($run.Code): $($run.Output.Trim())" } }
    $tgz = Join-Path $dir 'vibdock-v9.9.9-x86_64-unknown-linux-gnu.tar.gz'
    if (-not (Test-Path -LiteralPath $tgz)) { return @{ Success = $false; Detail = 'tar.gz not produced' } }
    $listing = (& tar -tzf $tgz) -join ' '
    if ($listing -notmatch 'vibdock') { return @{ Success = $false; Detail = "CLI missing: $listing" } }
    if ($listing -notmatch 'README') { return @{ Success = $false; Detail = "README missing: $listing" } }
    if ($listing -match 'launcher') { return @{ Success = $false; Detail = "launcher wrongly packaged: $listing" } }
    $bytes = [IO.File]::ReadAllBytes("$tgz.sha256")
    if ($bytes[-1] -ne 0x0A -or $bytes[-2] -eq 0x0D) { return @{ Success = $false; Detail = 'sidecar not LF-terminated' } }
    return @{ Success = $true }
}

# 8. Unix missing CLI must fail (launcher must never be substituted).
Assert-Case 'unix missing CLI is rejected' {
    $dir = New-Case 'unix-missing-cli' $unixTarget
    $run = Invoke-Package $dir $unixTarget 'vibdock' $null
    return Assert-FailsWith $run 'missing'
}

Write-Host ""
Write-Host "package-release acceptance: $passed passed, $failed failed"
if ($failed -gt 0) { exit 1 }
exit 0
