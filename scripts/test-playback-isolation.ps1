<#
.SYNOPSIS
    Opt-in Windows two-application playback isolation harness.

.DESCRIPTION
    Reproduces the manual check recorded for issue #41 against real Windows
    Core Audio playback sessions:

      1. generate two short distinct WAV tones in a temporary directory;
      2. start two real player processes (pwsh + System.Media.SoundPlayer)
         and capture their PIDs;
      3. require exactly one active render endpoint from `vibdock app-devices`;
      4. read `vibdock app-list --endpoint <id>` and select exactly the two
         sessions owned by those PIDs;
      5. `app-mute` session A, then assert `app-status` reports A muted=true and
         B muted=false, active=true;
      6. `app-unmute` A and assert A muted=false;
      7. stop both players and delete the temporary directory in `finally`.

    The endpoint and session identities are chosen only by exact observed
    identity: one endpoint, one session per player PID, re-verified by every
    CLI readback. If any part is missing or ambiguous the harness fails instead
    of falling back to a default endpoint, another session, or device-level
    mute. It never issues a device/microphone command, so the endpoint master
    mute and microphone state are not touched.

    This is state-level isolation evidence. No meeting is joined and audible
    output is not measured; it does not prove meeting-level audio isolation.

    Do not run this in hosted CI: hosted runners have no usable audio hardware.
    It is an opt-in desktop acceptance harness only.

.PARAMETER Binary
    Built vibdock executable. Relative paths are resolved against the
    repository root. Default: target\debug\vibdock.exe

.PARAMETER TimeoutSeconds
    How long to wait for both player playback sessions to appear and become
    active. Default: 20 seconds.

.EXAMPLE
    cargo build --locked
    pwsh -NoProfile -File scripts/test-playback-isolation.ps1

.EXAMPLE
    pwsh -NoProfile -File scripts/test-playback-isolation.ps1 `
        -Binary .\target\debug\vibdock.exe -TimeoutSeconds 30
#>
#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$Binary = 'target\debug\vibdock.exe',
    [ValidateRange(5, 120)]
    [int]$TimeoutSeconds = 20
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) {
    throw 'This harness requires Windows: application playback sessions use the Windows Core Audio adapter.'
}
if ($env:GITHUB_ACTIONS -eq 'true') {
    throw 'Refusing to run in hosted GitHub Actions: hosted runners have no audio hardware. This opt-in harness is for interactive Windows desktops only.'
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$binaryPath = if ([IO.Path]::IsPathRooted($Binary)) { $Binary } else { Join-Path $repoRoot $Binary }
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "Built binary not found: $binaryPath. Run 'cargo build --locked' first, or pass -Binary."
}
$binaryPath = (Resolve-Path -LiteralPath $binaryPath).Path
$powerShellPath = [Environment]::ProcessPath
if ([string]::IsNullOrWhiteSpace($powerShellPath)) {
    $powerShellPath = Join-Path $PSHOME 'pwsh.exe'
}

$script:binaryPath = $binaryPath
$script:powerShellPath = $powerShellPath
$script:playerScriptPath = $null
$script:commands = [Collections.Generic.List[string]]::new()

$playerA = $null
$playerB = $null
$tempDir = $null
$endpointId = $null
$sessionAId = $null
$aMuted = $false

function Assert-SessionScopedCommand {
    param([Parameter(Mandatory)][string[]]$Arguments)

    # Hard allowlist. A bug in this harness must not be able to reach the
    # device-level verbs (`mute`, `unmute`, `toggle`, `status`, `devices`) or a
    # microphone endpoint, which is what would make a master-mute change
    # possible.
    $allowed = @('app-devices', 'app-list', 'app-status', 'app-mute', 'app-unmute')
    if ($Arguments.Count -eq 0 -or $Arguments[0] -notin $allowed) {
        throw "Refusing to run '$($Arguments -join ' ')': this harness may only use $($allowed -join ', '). Device-level and microphone commands are never allowed."
    }
    if ($Arguments -contains '--device') {
        throw "Refusing to run '$($Arguments -join ' ')': --device would leave the application-session scope."
    }
}

function Invoke-Vibdock {
    param([Parameter(Mandatory)][string[]]$Arguments)

    Assert-SessionScopedCommand -Arguments $Arguments
    $script:commands.Add(($Arguments -join ' '))

    $info = [Diagnostics.ProcessStartInfo]::new($script:binaryPath)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $info.ArgumentList.Add($argument)
    }

    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
    } finally {
        $process.Dispose()
    }

    [pscustomobject]@{
        Command  = $Arguments -join ' '
        ExitCode = $exitCode
        Stdout   = $stdout
        Stderr   = $stderr
    }
}

function Get-LastJsonLine {
    param(
        [Parameter(Mandatory)]$Result,
        [Parameter(Mandatory)][string]$Label
    )

    $jsonLine = @($Result.Stdout -split "\r?\n" |
        Where-Object { $_.TrimStart().StartsWith('{') } |
        Select-Object -Last 1)
    if ($jsonLine.Count -eq 0) {
        throw "$Label produced no JSON evidence. stdout: $($Result.Stdout.Trim()) stderr: $($Result.Stderr.Trim())"
    }
    $raw = $jsonLine[0].Trim()
    try {
        $parsed = $raw | ConvertFrom-Json
    } catch {
        throw "$Label produced invalid JSON evidence: $raw"
    }
    [pscustomobject]@{ Raw = $raw; Parsed = $parsed }
}

function Get-EndpointIds {
    param([Parameter(Mandatory)]$Result)

    # `app-devices` prints each endpoint as a friendly-name line followed by an
    # indented identifier line. Names are not identities; only the indented id
    # is used.
    $ids = [Collections.Generic.List[string]]::new()
    foreach ($line in ($Result.Stdout -split "\r?\n")) {
        if ($line -match '^  (\S.*?)\s*$') {
            $ids.Add($Matches[1])
        }
    }
    $ids.ToArray()
}

function Get-ListWarnings {
    param([Parameter(Mandatory)]$Result)

    @($Result.Stdout -split "\r?\n" | Where-Object { $_ -match '^warning:' })
}

function Find-PlayerSession {
    param(
        [Parameter(Mandatory)]$Sessions,
        [Parameter(Mandatory)][int]$ProcessId
    )

    @($Sessions | Where-Object {
            $null -ne $_.process_id -and [int]$_.process_id -eq $ProcessId
        })
}

function Wait-ForPlayerSessions {
    param(
        [Parameter(Mandatory)][string]$Endpoint,
        [Parameter(Mandatory)][int]$PidA,
        [Parameter(Mandatory)][int]$PidB,
        [Parameter(Mandatory)][int]$Timeout,
        [Parameter(Mandatory)]$PlayerA,
        [Parameter(Mandatory)]$PlayerB
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($Timeout)
    while ($true) {
        foreach ($player in @($PlayerA, $PlayerB)) {
            if ($player.HasExited) {
                throw "Player pid $($player.Id) exited with code $($player.ExitCode) before its playback session appeared."
            }
        }

        $result = Invoke-Vibdock -Arguments @('app-list', '--endpoint', $Endpoint)
        if ($result.ExitCode -ne 0) {
            throw "app-list failed with exit code $($result.ExitCode): $($result.Stderr.Trim()) $($result.Stdout.Trim())"
        }
        $evidence = Get-LastJsonLine -Result $result -Label 'app-list'
        $sessions = @($evidence.Parsed.sessions)
        $aMatches = @(Find-PlayerSession -Sessions $sessions -ProcessId $PidA)
        $bMatches = @(Find-PlayerSession -Sessions $sessions -ProcessId $PidB)

        # Two sessions claiming one player PID is ambiguous platform state.
        # Refuse rather than acting on whichever happened to be first.
        if ($aMatches.Count -gt 1) {
            throw "Refusing to guess: pid $PidA owns $($aMatches.Count) playback sessions. app-list JSON: $($evidence.Raw)"
        }
        if ($bMatches.Count -gt 1) {
            throw "Refusing to guess: pid $PidB owns $($bMatches.Count) playback sessions. app-list JSON: $($evidence.Raw)"
        }

        if ($aMatches.Count -eq 1 -and $bMatches.Count -eq 1 -and
            [bool]$aMatches[0].active -and [bool]$bMatches[0].active) {
            return [pscustomobject]@{
                Result   = $result
                Evidence = $evidence
                Sessions = $sessions
                A        = $aMatches[0]
                B        = $bMatches[0]
            }
        }

        if ([DateTime]::UtcNow -ge $deadline) {
            $detail = if ($sessions.Count -gt 0) { $evidence.Raw } else { '{"sessions":[]}' }
            throw "Both player playback sessions did not appear active within $Timeout seconds (pid $PidA matched $($aMatches.Count), pid $PidB matched $($bMatches.Count)). Last app-list JSON: $detail"
        }
        Start-Sleep -Milliseconds 500
    }
}

function Assert-SessionState {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][string]$Label,
        [Parameter(Mandatory)][string]$Endpoint,
        [Parameter(Mandatory)][string]$SessionId,
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][bool]$Muted,
        [Parameter(Mandatory)][bool]$Active
    )

    $problems = [Collections.Generic.List[string]]::new()
    if ([string]$Session.endpoint_id -ne $Endpoint) {
        $problems.Add("endpoint_id '$($Session.endpoint_id)' != '$Endpoint'")
    }
    if ([string]$Session.session_instance_id -ne $SessionId) {
        $problems.Add("session_instance_id '$($Session.session_instance_id)' != '$SessionId'")
    }
    if ($null -eq $Session.process_id) {
        $problems.Add('process_id is null (owner not resolved)')
    } elseif ([int]$Session.process_id -ne $ProcessId) {
        $problems.Add("process_id $($Session.process_id) != $ProcessId")
    }
    if ([bool]$Session.muted -ne $Muted) {
        $problems.Add("muted=$($Session.muted) expected $Muted")
    }
    if ([bool]$Session.active -ne $Active) {
        $problems.Add("active=$($Session.active) expected $Active")
    }

    if ($problems.Count -gt 0) {
        throw "Assertion failed for session $Label`: $($problems -join '; '). Evidence: $($Session | ConvertTo-Json -Compress)"
    }
    Write-Host "PASS: $Label pid=$ProcessId session=$SessionId muted=$($Session.muted) active=$($Session.active)"
}

function Get-SessionStatus {
    param(
        [Parameter(Mandatory)][string]$Endpoint,
        [Parameter(Mandatory)][string]$SessionId,
        [Parameter(Mandatory)][string]$Label
    )

    $result = Invoke-Vibdock -Arguments @('app-status', '--endpoint', $Endpoint, '--session', $SessionId)
    if ($result.ExitCode -ne 0) {
        throw "app-status for $Label failed with exit code $($result.ExitCode): $($result.Stderr.Trim()) $($result.Stdout.Trim())"
    }
    $evidence = Get-LastJsonLine -Result $result -Label "app-status $Label"
    Write-Host "$Label app-status JSON: $($evidence.Raw)"
    $evidence
}

function Write-ToneWav {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][double]$Frequency,
        [double]$Seconds = 0.8,
        [int]$SampleRate = 44100,
        [double]$Amplitude = 0.2
    )

    $sampleCount = [int]($SampleRate * $Seconds)
    $dataBytes = $sampleCount * 2
    $fadeSamples = [int]($SampleRate * 0.05)
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    try {
        $writer = [IO.BinaryWriter]::new($stream)
        try {
            $writer.Write([Text.Encoding]::ASCII.GetBytes('RIFF'))
            $writer.Write([int](36 + $dataBytes))
            $writer.Write([Text.Encoding]::ASCII.GetBytes('WAVE'))
            $writer.Write([Text.Encoding]::ASCII.GetBytes('fmt '))
            $writer.Write([int]16)
            $writer.Write([int16]1)
            $writer.Write([int16]1)
            $writer.Write([int]$SampleRate)
            $writer.Write([int]($SampleRate * 2))
            $writer.Write([int16]2)
            $writer.Write([int16]16)
            $writer.Write([Text.Encoding]::ASCII.GetBytes('data'))
            $writer.Write([int]$dataBytes)
            for ($index = 0; $index -lt $sampleCount; $index++) {
                $envelope = 1.0
                if ($index -lt $fadeSamples) {
                    $envelope = $index / $fadeSamples
                } elseif ($index -ge ($sampleCount - $fadeSamples)) {
                    $envelope = ($sampleCount - 1 - $index) / $fadeSamples
                }
                $angle = 2.0 * [Math]::PI * $Frequency * ($index / $SampleRate)
                $sample = [int16][Math]::Round(32767.0 * $Amplitude * $envelope * [Math]::Sin($angle))
                $writer.Write($sample)
            }
        } finally {
            $writer.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

function Start-Player {
    param(
        [Parameter(Mandatory)][string]$WavPath,
        [Parameter(Mandatory)][string]$Label
    )

    $info = [Diagnostics.ProcessStartInfo]::new($script:powerShellPath)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', $script:playerScriptPath, $WavPath)) {
        $info.ArgumentList.Add($argument)
    }
    $process = [Diagnostics.Process]::Start($info)
    Write-Host "Player ${Label}: pid $($process.Id) wav $WavPath"
    $process
}

function Stop-Player {
    param($Process, [string]$Label)

    if ($null -eq $Process) { return }
    try {
        if (-not $Process.HasExited) {
            try {
                $Process.Kill($true)
            } catch {
                $Process.Kill()
            }
            if (-not $Process.WaitForExit(5000)) {
                Write-Warning "Player $Label pid $($Process.Id) did not exit within 5 seconds."
            }
        }
        Write-Host "Stopped player ${Label}: pid $($Process.Id) exited=$($Process.HasExited)"
    } catch {
        Write-Warning "Could not stop player $Label cleanly: $($_.Exception.Message)"
    } finally {
        $Process.Dispose()
    }
}

$tempDir = Join-Path ([IO.Path]::GetTempPath()) ('vibdock-playback-isolation-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempDir | Out-Null
$toneA = Join-Path $tempDir 'tone-a-440hz.wav'
$toneB = Join-Path $tempDir 'tone-b-660hz.wav'
$script:playerScriptPath = Join-Path $tempDir 'player.ps1'

try {
    Write-Host 'VibDock two-application playback isolation harness'
    Write-Host "Binary: $binaryPath"
    Write-Host "Temp directory: $tempDir"

    Set-Content -LiteralPath $script:playerScriptPath -Encoding utf8 -Value @'
param(
    [Parameter(Mandatory)][string]$Path
)
$ErrorActionPreference = 'Stop'
$player = [System.Media.SoundPlayer]::new($Path)
$player.Load()
$player.PlayLooping()
try {
    while ($true) { Start-Sleep -Seconds 1 }
} finally {
    $player.Stop()
}
'@

    Write-Host 'Generating two distinct WAV tones...'
    Write-ToneWav -Path $toneA -Frequency 440
    Write-ToneWav -Path $toneB -Frequency 660
    Write-Host "Tone A: 440 Hz -> $toneA"
    Write-Host "Tone B: 660 Hz -> $toneB"

    $playerA = Start-Player -WavPath $toneA -Label 'A'
    $playerB = Start-Player -WavPath $toneB -Label 'B'

    $devicesResult = Invoke-Vibdock -Arguments @('app-devices')
    if ($devicesResult.ExitCode -ne 0) {
        throw "app-devices failed with exit code $($devicesResult.ExitCode): $($devicesResult.Stderr.Trim()) $($devicesResult.Stdout.Trim())"
    }
    Write-Host "app-devices output:`n$($devicesResult.Stdout.TrimEnd())"

    $endpoints = @(Get-EndpointIds -Result $devicesResult)
    if ($endpoints.Count -ne 1) {
        throw "Expected exactly one active render endpoint, found $($endpoints.Count). Refusing to choose among them. Endpoints: $($endpoints -join ', ')"
    }
    $endpointId = $endpoints[0]
    Write-Host "Endpoint: $endpointId"

    $matched = Wait-ForPlayerSessions -Endpoint $endpointId -PidA $playerA.Id -PidB $playerB.Id `
        -Timeout $TimeoutSeconds -PlayerA $playerA -PlayerB $playerB
    $sessionAId = [string]$matched.A.session_instance_id
    $sessionBId = [string]$matched.B.session_instance_id
    Write-Host "Session A: $sessionAId (pid $($playerA.Id))"
    Write-Host "Session B: $sessionBId (pid $($playerB.Id))"
    Write-Host "app-list JSON: $($matched.Evidence.Raw)"
    foreach ($warning in @(Get-ListWarnings -Result $matched.Result)) {
        Write-Host $warning
    }

    Assert-SessionState -Session $matched.A -Label 'A (initial)' -Endpoint $endpointId `
        -SessionId $sessionAId -ProcessId $playerA.Id -Muted $false -Active $true
    Assert-SessionState -Session $matched.B -Label 'B (initial)' -Endpoint $endpointId `
        -SessionId $sessionBId -ProcessId $playerB.Id -Muted $false -Active $true

    Write-Host 'Muting session A with app-mute...'
    $muteResult = Invoke-Vibdock -Arguments @('app-mute', '--endpoint', $endpointId, '--session', $sessionAId)
    if ($muteResult.ExitCode -ne 0) {
        throw "app-mute for A failed with exit code $($muteResult.ExitCode): $($muteResult.Stderr.Trim()) $($muteResult.Stdout.Trim())"
    }
    $muteEvidence = Get-LastJsonLine -Result $muteResult -Label 'app-mute A'
    $aMuted = $true
    Write-Host "app-mute A JSON: $($muteEvidence.Raw)"
    Assert-SessionState -Session $muteEvidence.Parsed -Label 'A (app-mute readback)' -Endpoint $endpointId `
        -SessionId $sessionAId -ProcessId $playerA.Id -Muted $true -Active $true

    $statusAMuted = Get-SessionStatus -Endpoint $endpointId -SessionId $sessionAId -Label 'A after mute'
    Assert-SessionState -Session $statusAMuted.Parsed -Label 'A (app-status)' -Endpoint $endpointId `
        -SessionId $sessionAId -ProcessId $playerA.Id -Muted $true -Active $true
    $statusBWhileAMuted = Get-SessionStatus -Endpoint $endpointId -SessionId $sessionBId -Label 'B while A is muted'
    Assert-SessionState -Session $statusBWhileAMuted.Parsed -Label 'B (app-status)' -Endpoint $endpointId `
        -SessionId $sessionBId -ProcessId $playerB.Id -Muted $false -Active $true

    Write-Host 'Unmuting session A with app-unmute...'
    $unmuteResult = Invoke-Vibdock -Arguments @('app-unmute', '--endpoint', $endpointId, '--session', $sessionAId)
    if ($unmuteResult.ExitCode -ne 0) {
        throw "app-unmute for A failed with exit code $($unmuteResult.ExitCode): $($unmuteResult.Stderr.Trim()) $($unmuteResult.Stdout.Trim())"
    }
    $unmuteEvidence = Get-LastJsonLine -Result $unmuteResult -Label 'app-unmute A'
    $aMuted = $false
    Write-Host "app-unmute A JSON: $($unmuteEvidence.Raw)"
    Assert-SessionState -Session $unmuteEvidence.Parsed -Label 'A (app-unmute readback)' -Endpoint $endpointId `
        -SessionId $sessionAId -ProcessId $playerA.Id -Muted $false -Active $true

    $statusAUnmuted = Get-SessionStatus -Endpoint $endpointId -SessionId $sessionAId -Label 'A after unmute'
    Assert-SessionState -Session $statusAUnmuted.Parsed -Label 'A (app-status after unmute)' -Endpoint $endpointId `
        -SessionId $sessionAId -ProcessId $playerA.Id -Muted $false -Active $true
    $statusBFinal = Get-SessionStatus -Endpoint $endpointId -SessionId $sessionBId -Label 'B final'
    Assert-SessionState -Session $statusBFinal.Parsed -Label 'B (app-status final)' -Endpoint $endpointId `
        -SessionId $sessionBId -ProcessId $playerB.Id -Muted $false -Active $true

    # A final enumeration proves both exact sessions are still present unmuted,
    # not merely that the last status read was clean.
    $finalListResult = Invoke-Vibdock -Arguments @('app-list', '--endpoint', $endpointId)
    if ($finalListResult.ExitCode -ne 0) {
        throw "final app-list failed with exit code $($finalListResult.ExitCode): $($finalListResult.Stderr.Trim()) $($finalListResult.Stdout.Trim())"
    }
    $finalEvidence = Get-LastJsonLine -Result $finalListResult -Label 'final app-list'
    Write-Host "Final app-list JSON: $($finalEvidence.Raw)"
    $finalSessions = @($finalEvidence.Parsed.sessions)
    $finalA = @(Find-PlayerSession -Sessions $finalSessions -ProcessId $playerA.Id)
    $finalB = @(Find-PlayerSession -Sessions $finalSessions -ProcessId $playerB.Id)
    if ($finalA.Count -ne 1 -or $finalB.Count -ne 1) {
        throw "Final app-list did not contain exactly one session per player PID (A=$($finalA.Count), B=$($finalB.Count)). JSON: $($finalEvidence.Raw)"
    }
    Assert-SessionState -Session $finalA[0] -Label 'A (final enumeration)' -Endpoint $endpointId `
        -SessionId $sessionAId -ProcessId $playerA.Id -Muted $false -Active $true
    Assert-SessionState -Session $finalB[0] -Label 'B (final enumeration)' -Endpoint $endpointId `
        -SessionId $sessionBId -ProcessId $playerB.Id -Muted $false -Active $true

    $summary = [ordered]@{
        result                = 'PASS'
        binary                = $binaryPath
        endpoint_id           = $endpointId
        endpoint_master_mute  = 'untouched'
        endpoint_scope_note   = 'Only app-* session commands were issued; no device-level mute/unmute/toggle/status or microphone command was run.'
        commands_issued       = @($script:commands)
        players               = [ordered]@{
            A = [ordered]@{ pid = $playerA.Id; tone_hz = 440; session_instance_id = $sessionAId }
            B = [ordered]@{ pid = $playerB.Id; tone_hz = 660; session_instance_id = $sessionBId }
        }
        raw_evidence          = [ordered]@{
            app_devices               = $devicesResult.Stdout.Trim()
            app_list_initial          = $matched.Evidence.Raw
            app_mute_a                = $muteEvidence.Raw
            app_status_a_after_mute   = $statusAMuted.Raw
            app_status_b_while_a_muted = $statusBWhileAMuted.Raw
            app_unmute_a              = $unmuteEvidence.Raw
            app_status_a_after_unmute = $statusAUnmuted.Raw
            app_status_b_final        = $statusBFinal.Raw
            app_list_final            = $finalEvidence.Raw
        }
    }
    Write-Host ''
    Write-Output ($summary | ConvertTo-Json -Depth 12)
    Write-Host 'PASS: two-session state isolation verified; endpoint master mute untouched.'
} catch {
    Write-Host ''
    Write-Host "FAIL: $($_.Exception.Message)" -ForegroundColor Red
    throw
} finally {
    # State cleanup first: A must be unmuted while its player session still
    # exists. Best effort only; never mask the original failure.
    if ($aMuted -and $endpointId -and $sessionAId) {
        try {
            $cleanup = Invoke-Vibdock -Arguments @('app-unmute', '--endpoint', $endpointId, '--session', $sessionAId)
            if ($cleanup.ExitCode -eq 0) {
                Write-Host 'Cleanup: session A unmuted before stopping players.'
            } else {
                Write-Warning "Cleanup: session A could not be unmuted: $($cleanup.Stderr.Trim())"
            }
        } catch {
            Write-Warning "Cleanup: session A could not be unmuted: $($_.Exception.Message)"
        }
    }
    Stop-Player -Process $playerA -Label 'A'
    Stop-Player -Process $playerB -Label 'B'
    if ($tempDir -and (Test-Path -LiteralPath $tempDir)) {
        try {
            Remove-Item -LiteralPath $tempDir -Recurse -Force
            Write-Host "Cleanup: removed $tempDir"
        } catch {
            Write-Warning "Cleanup: could not remove ${tempDir}: $($_.Exception.Message)"
        }
    }
}
