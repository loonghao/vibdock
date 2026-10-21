$ErrorActionPreference = 'Stop'
$env:LOCALAPPDATA = Join-Path $PSScriptRoot 'preview-data'
$env:VIBDOCK_PORT = '38942'
& (Join-Path $PSScriptRoot 'vibdock-launcher.exe')
