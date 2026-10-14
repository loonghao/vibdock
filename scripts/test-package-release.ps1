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
$verifyScript = Join-Path $PSScriptRoot 'verify-windows-package.ps1'
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

function Invoke-PackageRaw([string]$Dir, [string]$Tag, [string]$Target, [string]$Cli, [string]$Launcher) {
    $a = @('-NoProfile', '-File', $script, '-Tag', $Tag, '-Target', $Target,
           '-CliExecutable', $Cli, '-OutputDirectory', $Dir)
    if ($Launcher) { $a += @('-LauncherExecutable', $Launcher) }
    $out = & $pwsh @a 2>&1
    return @{ Code = $LASTEXITCODE; Output = ($out | Out-String) }
}

function Invoke-Package([string]$Dir, [string]$Target, [string]$Cli, [string]$Launcher) {
    return Invoke-PackageRaw $Dir 'v9.9.9' $Target $Cli $Launcher
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

# ---- Producer hardening: untrusted Tag/Target/executable names must be rejected
# before any path is built or deleted, and must never touch anything outside the
# output directory. Proven non-destructively: a sibling canary must survive, no
# archive may be produced, and the failure must name the validation reason.

# 9. Tag with a path separator is rejected; sibling canary survives; no archive.
Assert-Case 'producer rejects Tag with path separator (canary survives)' {
    $dir = New-Case 'producer-tag-sep' $unixTarget
    Set-Content -LiteralPath (Join-Path $dir "target/$unixTarget/release/vibdock") -Value 'cli' -NoNewline
    $canary = Join-Path $root 'canary-tag-sep'
    New-Item -ItemType Directory -Path $canary -Force | Out-Null
    $sentinel = Join-Path $canary 'keep.txt'
    Set-Content -LiteralPath $sentinel -Value 'keep' -NoNewline
    $run = Invoke-PackageRaw $dir 'a/b' $unixTarget 'vibdock' $null
    $rejected = Assert-FailsWith $run 'path separators'
    if (-not $rejected.Success) { return $rejected }
    if (-not (Test-Path -LiteralPath $sentinel)) { return @{ Success = $false; Detail = 'canary was deleted (destructive traversal)' } }
    if (Get-ChildItem -LiteralPath $dir -Filter '*.tar.gz' -ErrorAction SilentlyContinue) { return @{ Success = $false; Detail = 'archive produced despite invalid Tag' } }
    return @{ Success = $true }
}

# 10. Target carrying '..' traversal is rejected; sibling canary survives.
Assert-Case 'producer rejects Target with parent-dir traversal (canary survives)' {
    $dir = New-Case 'producer-target-traversal' $unixTarget
    New-Item -ItemType Directory -Path (Join-Path $dir "target/$unixTarget/release") -Force | Out-Null
    $canary = Join-Path $root 'canary-target-trav'
    New-Item -ItemType Directory -Path $canary -Force | Out-Null
    $sentinel = Join-Path $canary 'keep.txt'
    Set-Content -LiteralPath $sentinel -Value 'keep' -NoNewline
    $run = Invoke-PackageRaw $dir 'v9.9.9' 'x\..\..\pwned' 'vibdock' $null
    $rejected = Assert-FailsWith $run 'path separators'
    if (-not $rejected.Success) { return $rejected }
    if (-not (Test-Path -LiteralPath $sentinel)) { return @{ Success = $false; Detail = 'canary was deleted (destructive traversal)' } }
    return @{ Success = $true }
}

# 11. A bare '..' Tag (parent-dir reference, no separator) is rejected.
Assert-Case 'producer rejects parent-directory Tag' {
    $dir = New-Case 'producer-tag-dotdot' $unixTarget
    Set-Content -LiteralPath (Join-Path $dir "target/$unixTarget/release/vibdock") -Value 'cli' -NoNewline
    $run = Invoke-PackageRaw $dir '..' $unixTarget 'vibdock' $null
    return Assert-FailsWith $run 'parent-directory'
}

# 12. An executable name with a separator is rejected (guards the read path).
Assert-Case 'producer rejects CliExecutable with path separator' {
    $dir = New-Case 'producer-cli-sep' $winTarget
    New-Item -ItemType Directory -Path (Join-Path $dir "target/$winTarget/release") -Force | Out-Null
    $run = Invoke-PackageRaw $dir 'v9.9.9' $winTarget 'sub\vibdock.exe' 'vibdock-launcher.exe'
    return Assert-FailsWith $run 'path separators'
}

# ---- Verifier hardening: a reused evidence directory must not let stale files
# from a prior good archive satisfy a later archive's checks.

function New-WindowsArchive([string]$Out, [bool]$WithLauncher, [int]$LauncherSubsystem = 2, [bool]$WithReadme = $true) {
    if (Test-Path -LiteralPath $Out) { Remove-Item -LiteralPath $Out -Force }
    $stage = Join-Path $root ('zipstage-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    New-PeStub (Join-Path $stage 'vibdock.exe') 3
    if ($WithLauncher) { New-PeStub (Join-Path $stage 'vibdock-launcher.exe') $LauncherSubsystem }
    if ($WithReadme) { Set-Content -LiteralPath (Join-Path $stage 'README.md') -Value 'VibDock' -NoNewline }
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $Out -Force
    Remove-Item -Recurse -Force -LiteralPath $stage
}

function Invoke-Verify([string]$Archive, [string]$EvidenceDir) {
    $a = @('-NoProfile', '-File', $verifyScript, '-Archive', $Archive, '-EvidenceDirectory', $EvidenceDir)
    $out = & $pwsh @a 2>&1
    return @{ Code = $LASTEXITCODE; Output = ($out | Out-String) }
}

# 13. Verifier accepts a complete Windows archive.
Assert-Case 'verifier accepts complete windows archive' {
    $dir = New-Case 'verify-complete' $winTarget
    $zip = Join-Path $dir 'complete.zip'
    New-WindowsArchive $zip $true 2
    $run = Invoke-Verify $zip (Join-Path $dir 'evidence')
    if ($run.Code -ne 0) { return @{ Success = $false; Detail = "verify exited $($run.Code): $($run.Output.Trim())" } }
    return @{ Success = $true }
}

# 14. Verifier rejects a launcher-less archive even into a REUSED evidence dir
# that still holds a launcher from a prior good archive (false-PASS regression).
Assert-Case 'verifier rejects missing-launcher in reused evidence dir' {
    $dir = New-Case 'verify-reuse-missing' $winTarget
    $ev = Join-Path $dir 'reused-evidence'
    $complete = Join-Path $dir 'complete.zip'
    $missing = Join-Path $dir 'missing-launcher.zip'
    New-WindowsArchive $complete $true 2
    New-WindowsArchive $missing $false
    $good = Invoke-Verify $complete $ev
    if ($good.Code -ne 0) { return @{ Success = $false; Detail = "primer verify failed: $($good.Output.Trim())" } }
    $bad = Invoke-Verify $missing $ev
    if ($bad.Code -eq 0) { return @{ Success = $false; Detail = 'missing-launcher FALSE-PASSED in reused evidence dir' } }
    if ($bad.Output -notmatch 'launcher') { return @{ Success = $false; Detail = "unexpected error: $($bad.Output.Trim())" } }
    return @{ Success = $true }
}

# 15. Verifier rejects a console-subsystem launcher even after a prior good
# archive in the SAME reused evidence dir.
Assert-Case 'verifier rejects wrong-subsystem launcher in reused evidence dir' {
    $dir = New-Case 'verify-reuse-subsystem' $winTarget
    $ev = Join-Path $dir 'reused-evidence'
    $complete = Join-Path $dir 'complete.zip'
    $wrong = Join-Path $dir 'wrong-subsystem.zip'
    New-WindowsArchive $complete $true 2
    New-WindowsArchive $wrong $true 3
    $good = Invoke-Verify $complete $ev
    if ($good.Code -ne 0) { return @{ Success = $false; Detail = "primer verify failed: $($good.Output.Trim())" } }
    $bad = Invoke-Verify $wrong $ev
    if ($bad.Code -eq 0) { return @{ Success = $false; Detail = 'console-subsystem launcher FALSE-PASSED in reused evidence dir' } }
    if ($bad.Output -notmatch 'GUI subsystem') { return @{ Success = $false; Detail = "unexpected error: $($bad.Output.Trim())" } }
    return @{ Success = $true }
}

# 16. Verifier rejects a README-less archive in a reused evidence dir (a stale
# README from a prior archive must not satisfy the README requirement).
Assert-Case 'verifier rejects missing-README in reused evidence dir' {
    $dir = New-Case 'verify-reuse-readme' $winTarget
    $ev = Join-Path $dir 'reused-evidence'
    $complete = Join-Path $dir 'complete.zip'
    $noReadme = Join-Path $dir 'no-readme.zip'
    New-WindowsArchive $complete $true 2
    New-WindowsArchive $noReadme $true 2 $false
    $good = Invoke-Verify $complete $ev
    if ($good.Code -ne 0) { return @{ Success = $false; Detail = "primer verify failed: $($good.Output.Trim())" } }
    $bad = Invoke-Verify $noReadme $ev
    if ($bad.Code -eq 0) { return @{ Success = $false; Detail = 'missing-README FALSE-PASSED in reused evidence dir' } }
    if ($bad.Output -notmatch 'README') { return @{ Success = $false; Detail = "unexpected error: $($bad.Output.Trim())" } }
    return @{ Success = $true }
}

Write-Host ""
Write-Host "package-release acceptance: $passed passed, $failed failed"
if ($failed -gt 0) { exit 1 }
exit 0
