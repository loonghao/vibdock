$ErrorActionPreference = 'Stop'
$env:LOCALAPPDATA = Join-Path $PSScriptRoot 'preview-data'
$env:VIBDOCK_PORT = '38942'
# Prevent first-run built-in firmware defaults in this isolated phone preview.
$stateDirectory = Join-Path $env:LOCALAPPDATA 'VibDock'
New-Item -ItemType Directory -Force $stateDirectory | Out-Null
$hardwareFile = Join-Path $stateDirectory 'hardware.json'
if (-not (Test-Path -LiteralPath $hardwareFile)) {
    [IO.File]::WriteAllText($hardwareFile, '{"schema_version":1,"revision":0,"direct":[]}', [Text.UTF8Encoding]::new($false))
}
& (Join-Path $PSScriptRoot 'vibdock-launcher.exe')
