<#
Native Windows smoke acceptance, using real built executables and window handles.
Runs in an isolated state directory/port; no microphone is selected or controlled.
Window messages exercise tray lifecycle commands. PrintWindow captures real menu
rendering. Launch/relaunch open the default browser; browser acceptance is separate.
Only PNGs and a redacted report are written to EvidenceDirectory.
#>
param(
    [string]$BinaryDirectory = 'target/release',
    [string]$EvidenceDirectory = 'work/windows-desktop'
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This acceptance requires Windows.' }
$bin = (Resolve-Path -LiteralPath $BinaryDirectory).Path
foreach ($name in @('vibdock.exe', 'vibdock-launcher.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $bin $name))) { throw "Missing built $name" }
}
New-Item -ItemType Directory -Force -Path $EvidenceDirectory | Out-Null
$evidence = (Resolve-Path -LiteralPath $EvidenceDirectory).Path
foreach ($name in @('report.json', 'resident-panel.png', 'resident-context-menu.png', 'resident-software-menu.png', 'resident-result-overflow-fixture.png', 'resident-restored.png')) {
    $old = Join-Path $evidence $name
    if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Force }
    if ($name.EndsWith('.png')) {
        $oldBlank = Join-Path $evidence ([IO.Path]::GetFileNameWithoutExtension($name) + '-blank.png')
        if (Test-Path -LiteralPath $oldBlank) { Remove-Item -LiteralPath $oldBlank -Force }
    }
}
$state = Join-Path ([IO.Path]::GetTempPath()) ('vibdock-desktop-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $state | Out-Null
$portProbe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$portProbe.Start(); $port = $portProbe.LocalEndpoint.Port; $portProbe.Stop()
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class VibDockNative {
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr hwnd, int id);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetMenuString(IntPtr menu, uint index, StringBuilder text, int count, uint flags);
  [DllImport("user32.dll")] public static extern int GetMenuItemCount(IntPtr menu);
}
'@
function Start-Entry([string]$Name, [string]$Argument = '') {
    $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $bin $Name))
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    if ($Argument) { $info.ArgumentList.Add($Argument) }
    $info.Environment['LOCALAPPDATA'] = $state
    $info.Environment['VIBDOCK_PORT'] = [string]$port
    $info.Environment['VIBDOCK_OVERLAY'] = '0'
    return [Diagnostics.Process]::Start($info)
}
function Wait-Until([scriptblock]$Condition, [string]$Failure) {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do { if (& $Condition) { return }; Start-Sleep -Milliseconds 100 } while ([DateTime]::UtcNow -lt $deadline)
    throw $Failure
}
function Save-Window([IntPtr]$Handle, [string]$Name) {
    # Menu handles/labels can exist before their opening animation has painted.
    # Retry capture readiness only, never an action or a semantic assertion.
    $captureWait = [Diagnostics.Stopwatch]::StartNew()
    do {
        [uint32]$capturePid = 0
        [void][VibDockNative]::GetWindowThreadProcessId($Handle, [ref]$capturePid)
        if ($capturePid -ne $first.Id) { throw 'Capture target no longer belongs to the tested launcher' }
        if (-not [VibDockNative]::IsWindowVisible($Handle)) { throw 'Capture target is no longer visible' }
        $rect = [VibDockNative+Rect]::new()
        if (-not [VibDockNative]::GetWindowRect($Handle, [ref]$rect)) { throw 'Window rectangle unavailable' }
        $width = $rect.Right - $rect.Left; $height = $rect.Bottom - $rect.Top
        if ($width -lt 100 -or $height -lt 30 -or $width -gt 2000 -or $height -gt 1200) { throw 'Unexpected capture bounds' }
        $bitmap = [Drawing.Bitmap]::new($width, $height)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $dc = $graphics.GetHdc()
            try { if (-not [VibDockNative]::PrintWindow($Handle, $dc, 2)) { throw 'Native PrintWindow failed' } }
            finally { $graphics.ReleaseHdc($dc) }
            $colors = [Collections.Generic.HashSet[int]]::new()
            for ($x = 0; $x -lt $width; $x += 3) { for ($y = 0; $y -lt $height; $y += 3) { [void]$colors.Add($bitmap.GetPixel($x,$y).ToArgb()) } }
            if ($colors.Count -ge 4) {
                $bitmap.Save((Join-Path $evidence $Name), [Drawing.Imaging.ImageFormat]::Png)
                return
            }
            if ($captureWait.ElapsedMilliseconds -ge 5000) {
                # Preserve the actual failed render for diagnosis; never label
                # it as a successful capture or weaken the blank-frame guard.
                $failedName = [IO.Path]::GetFileNameWithoutExtension($Name) + '-blank.png'
                $bitmap.Save((Join-Path $evidence $failedName), [Drawing.Imaging.ImageFormat]::Png)
                throw "Native capture stayed blank: $Name; visual acceptance is unavailable"
            }
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
        Start-Sleep -Milliseconds 100
    } while ($true)
}
# A saved offline fixture validates persistence rendering, not live app audio.
$fixtureDirectory = Join-Path $state 'VibDock'
New-Item -ItemType Directory -Path $fixtureDirectory | Out-Null
@{schema_version=1;revision=1;entries=@(@{id=('a' * 64);label='Offline saved fixture';endpoint_id='fixture-output';executable_path='C:\VibDockFixture\NeverExecuted.exe'})} | ConvertTo-Json -Depth 5 -Compress | Set-Content -LiteralPath (Join-Path $fixtureDirectory 'apps.json') -Encoding utf8NoBOM
$first = $null; $second = $null; $window = [IntPtr]::Zero
try {
    $first = Start-Entry 'vibdock-launcher.exe'
    Wait-Until { $script:window = [VibDockNative]::FindWindow('VibDockTray', 'VibDock'); $script:window -ne [IntPtr]::Zero } 'Tray owner did not start'
    [uint32]$windowPid = 0
    [void][VibDockNative]::GetWindowThreadProcessId($window, [ref]$windowPid)
    if ($windowPid -ne $first.Id) { throw 'Tray owner belongs to a different launcher; refusing to control it' }
    if ([VibDockNative]::IsWindowVisible($window)) { throw 'Obsolete resident panel is still visible' }
    foreach ($id in @(10, 11, 12, 13, 101, 105, 200, 201)) {
        if ([VibDockNative]::GetDlgItem($window, $id) -ne [IntPtr]::Zero) { throw 'Obsolete resident panel controls still exist' }
    }
    $recordPath = Join-Path $state 'VibDock/service.json'
    # Private comparison only: never print or upload this file/token.
    $initialRecord = [IO.File]::ReadAllText($recordPath)
    $serviceChildren = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($first.Id)" | Where-Object { $_.Name -eq 'vibdock.exe' } | Select-Object -ExpandProperty ProcessId)
    if ($serviceChildren.Count -ne 1) { throw 'Expected one owned service child for observer cleanup acceptance' }
    $privateRecord = $initialRecord | ConvertFrom-Json
    $script:softwareView = $null
    Wait-Until {
        try { $script:softwareView = Invoke-RestMethod -Uri ("http://127.0.0.1:$port/api/apps") -Headers @{Authorization=('Bearer ' + $privateRecord.token)} -TimeoutSec 3; return $script:softwareView.generation -gt 0 } catch { return $false }
    } 'Owned native observer did not publish a completed result'
    if ($softwareView.actions_enabled -or $softwareView.entries[0].state -ne 'unknown') { throw 'Offline fixture incorrectly enabled a playback control or invented live state' }
    if ($softwareView.error -match 'containment|activation|output storage|cleanup is unconfirmed|could not start|timed out') { throw 'Owned observer infrastructure failed' }
    $nativeProbe = @{generation=$softwareView.generation;available=$softwareView.available;complete=$softwareView.complete;readableSessions=@($softwareView.choices).Count;skipped=$softwareView.skipped;error=$softwareView.error}
    # Negative calls never name a live session or mutate audio. Raw targets and
    # forged/unknown capabilities must fail before the contained action helper.
    $prepareBody = @{entry_id=('a' * 64);session_reference=('f' * 64);action='mute'} | ConvertTo-Json -Compress
    $authHeaders = @{Authorization=('Bearer ' + $privateRecord.token)}
    $checks = @(
        @{path='/api/apps/actions/prepare';headers=@{};body=$prepareBody;expected=401},
        @{path='/api/apps/actions/prepare';headers=@{Authorization=('Bearer ' + $privateRecord.token);Origin='https://attacker.invalid'};body=$prepareBody;expected=403},
        @{path='/api/apps/actions/prepare';headers=$authHeaders;body=$prepareBody;expected=409},
        @{path='/api/apps/actions/prepare';headers=$authHeaders;body='{"entry_id":"fixture","session_reference":"forged","action":"mute","executable_path":"C:\\Fixture\\NeverExecuted.exe"}';expected=400},
        @{path='/api/apps/actions/execute';headers=$authHeaders;body=(@{operation_id=('e' * 64);capability=('f' * 64)} | ConvertTo-Json -Compress);expected=404}
    )
    foreach ($check in $checks) {
        $response = Invoke-WebRequest -Uri ("http://127.0.0.1:$port" + $check.path) -Method Post -Headers $check.headers -ContentType 'application/json' -Body $check.body -TimeoutSec 3 -SkipHttpErrorCheck
        if ([int]$response.StatusCode -ne $check.expected) { throw 'Playback action authorization or exact-selection rejection failed' }
    }
    [void][VibDockNative]::PostMessage($window, 0x007B, $window, [IntPtr](-1)) # WM_CONTEXTMENU
    $menuWindow = [IntPtr]::Zero
    Wait-Until { $script:menuWindow = [VibDockNative]::FindWindow('#32768', $null); $script:menuWindow -ne [IntPtr]::Zero } 'Context menu did not open'
    [uint32]$menuPid = 0
    [void][VibDockNative]::GetWindowThreadProcessId($menuWindow, [ref]$menuPid)
    if ($menuPid -ne $first.Id) { throw 'Context menu belongs to another process' }
    $menu = [VibDockNative]::SendMessage($menuWindow, 0x01E1, [IntPtr]::Zero, [IntPtr]::Zero) # MN_GETHMENU
    $labels = @()
    for ($i = 0; $i -lt [VibDockNative]::GetMenuItemCount($menu); $i++) {
        $label = [Text.StringBuilder]::new(160)
        [void][VibDockNative]::GetMenuString($menu, $i, $label, $label.Capacity, 0x400)
        $labels += $label.ToString()
    }
    if (@($labels).Count -ne 3 -or -not ($labels -contains 'Settings…') -or -not ($labels -contains 'Service status') -or -not ($labels -contains 'Exit VibDock')) { throw 'Context menu must contain only Settings, Service status, and Exit' }
    Save-Window $menuWindow 'resident-context-menu.png'
    [void][VibDockNative]::PostMessage($window, 0x001F, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CANCELMODE
    Start-Sleep -Milliseconds 150
    # Use the real first software icon uID, not the primary icon's generic menu.
    [void][VibDockNative]::PostMessage($window, 0x8001, [IntPtr]2, [IntPtr]0x0205)
    Wait-Until { $script:menuWindow = [VibDockNative]::FindWindow('#32768', $null); $script:menuWindow -ne [IntPtr]::Zero -and [VibDockNative]::IsWindowVisible($script:menuWindow) } 'Software icon context menu did not open'
    [uint32]$softwareMenuPid = 0
    [void][VibDockNative]::GetWindowThreadProcessId($menuWindow, [ref]$softwareMenuPid)
    if ($softwareMenuPid -ne $first.Id) { throw 'Software menu belongs to another process' }
    $menu = [VibDockNative]::SendMessage($menuWindow, 0x01E1, [IntPtr]::Zero, [IntPtr]::Zero)
    $softwareLabels = @()
    for ($i = 0; $i -lt [VibDockNative]::GetMenuItemCount($menu); $i++) {
        $label = [Text.StringBuilder]::new(160)
        [void][VibDockNative]::GetMenuString($menu, $i, $label, $label.Capacity, 0x400)
        $softwareLabels += $label.ToString()
    }
    if (-not ($softwareLabels -contains 'Offline saved fixture') -or -not ($softwareLabels -contains 'No fresh, controllable playback session') -or -not ($softwareLabels -contains 'Settings…')) { throw 'Software uID menu did not preserve its exact offline row and disabled controls' }
    Save-Window $menuWindow 'resident-software-menu.png'
    [void][VibDockNative]::PostMessage($window, 0x001F, [IntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 150
    [void][VibDockNative]::PostMessage($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CLOSE
    if ([VibDockNative]::IsWindowVisible($window) -or $first.HasExited -or -not (Test-Path -LiteralPath $recordPath)) { throw 'Close changed the hidden tray/service lifecycle' }
    $second = Start-Entry 'vibdock-launcher.exe'
    if (-not $second.WaitForExit(15000) -or $second.ExitCode -ne 0) { throw 'Second launcher did not hand over to the singleton' }
    if ($first.HasExited -or [IO.File]::ReadAllText($recordPath) -cne $initialRecord) { throw 'Repeated launch replaced the resident service' }
    # Primary click requests authenticated Web Settings, never a native panel.
    [void][VibDockNative]::PostMessage($window, 0x8001, [IntPtr]1, [IntPtr]0x0202) # WM_LBUTTONUP
    Start-Sleep -Milliseconds 500
    if ([VibDockNative]::IsWindowVisible($window)) { throw 'Launch or primary tray click revealed the obsolete panel' }
    [void][VibDockNative]::PostMessage($window, 0x0111, [IntPtr]103, [IntPtr]::Zero) # actual Exit command
    if (-not $first.WaitForExit(15000) -or $first.ExitCode -ne 0) { throw 'Exit did not finish cleanly' }
    Wait-Until { -not (Test-Path -LiteralPath $recordPath) } 'Exit left the service instance record behind'
    foreach ($servicePid in $serviceChildren) {
        Wait-Until { @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$servicePid" | Where-Object { $_.Name -eq 'vibdock.exe' }).Count -eq 0 } 'Exit left a native observation helper running'
    }
    $report = [ordered]@{ platform='Windows'; sourceRevision=$env:VIBDOCK_SOURCE_REVISION; launcherSHA256=(Get-FileHash -LiteralPath (Join-Path $bin 'vibdock-launcher.exe') -Algorithm SHA256).Hash; runtime=[Environment]::OSVersion.VersionString; launcherPid=$windowPid; windowHandle=$window.ToInt64(); port=$port; nativePlaybackProbe=$nativeProbe; livePlaybackControl='unverified: this acceptance does not produce or mutate representative real audio sessions'; checks=@('hidden-tray-owner-without-panel-controls','saved-offline-software-fixture-unknown','original-icon-resource-built','real-context-menu-settings-status-exit','software-icon-uid-menu-exact-offline-row','offline-session-controls-disabled','playback-action-auth-origin-forged-selection-rejected','close-keeps-tray-service-running','second-launch-reuses-singleton','primary-click-keeps-owner-hidden','service-record-unchanged','authenticated-exit-cleans-service','owned-native-observer-published','owned-observer-helpers-stopped'); input='Win32 messages (not physical mouse)'; captures='PrintWindow native menus'; exclusions=@('live application playback and third-party resource icons','microphone hardware','physical pointer/focus and multi-monitor DPI','macOS GUI','actual default-browser launch and authenticated page redemption') }
    $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $evidence 'report.json') -Encoding utf8
    Write-Output 'PASS: hidden tray owner, context menus, singleton reuse, and authenticated Exit. Captures are native menu renders; physical-device and default-browser acceptance remain separate.'
} finally {
    $stop = Start-Entry 'vibdock.exe' 'service-stop'
    if (-not $stop.WaitForExit(5000)) { $stop.Kill(); $stop.WaitForExit() }
    foreach ($process in @($second, $first)) { if ($null -ne $process -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit() } }
    # Do not discard credentials while an uncertain detached service may still
    # be running. Preserve the isolated state so authenticated cleanup can retry.
    if (Test-Path -LiteralPath (Join-Path $state 'VibDock/service.json')) {
        throw "Cleanup could not confirm service shutdown; isolated state retained at $state"
    }
    Remove-Item -LiteralPath $state -Recurse -Force
}
