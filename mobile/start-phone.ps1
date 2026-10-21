param(
    [Parameter(Mandatory=$true)][string]$Listen,
    [string[]]$AllowAction = @(),
    [switch]$AllowMicrophone,
    [string]$SttExecutable = '',
    [string]$ModelDirectory = ''
)
$ErrorActionPreference = 'Stop'
# This script changes only this process environment, never user-wide settings.
$env:LOCALAPPDATA = Join-Path $PSScriptRoot 'preview-data'
$env:VIBDOCK_PORT = '38942'
$arguments = @('--listen', $Listen, '--state', (Join-Path $env:LOCALAPPDATA 'VibDock/service.json'))
foreach ($id in $AllowAction) { $arguments += @('--allow-action', $id) }
if ($AllowMicrophone) { $arguments += '--allow-microphone' }
if ($SttExecutable) { $arguments += @('--stt-exe', $SttExecutable, '--model-dir', $ModelDirectory) }
& (Join-Path $PSScriptRoot 'vibdock-phone.exe') @arguments
exit $LASTEXITCODE
