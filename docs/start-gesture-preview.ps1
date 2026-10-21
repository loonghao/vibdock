$ErrorActionPreference = 'Stop'
$previewRoot = Split-Path -Parent $PSScriptRoot
$env:LOCALAPPDATA = Join-Path $previewRoot 'preview-data'
$env:VIBDOCK_PORT = '38941'
$stateDirectory = Join-Path $env:LOCALAPPDATA 'VibDock'
New-Item -ItemType Directory -Force $stateDirectory | Out-Null
$hardwareFile = Join-Path $stateDirectory 'hardware.json'
# Never seed the normal first-run firmware defaults into an existing keypad.
if (-not (Test-Path -LiteralPath $hardwareFile)) {
    [IO.File]::WriteAllText($hardwareFile, '{"schema_version":1,"revision":0,"direct":[]}', [Text.UTF8Encoding]::new($false))
}
& (Join-Path $previewRoot 'vibdock-launcher.exe')
