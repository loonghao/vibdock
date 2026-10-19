param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('plugins', 'actions', 'action-commands', 'hardware', 'config', 'pad-execute')]
    [string]$Command,
    [string]$Id
)

$ErrorActionPreference = 'Stop'
$statePath = Join-Path $env:LOCALAPPDATA 'VibDock\service.json'
if (-not (Test-Path -LiteralPath $statePath)) {
    throw "VibDock service state not found at $statePath. Start it with: vibdock service-start"
}

$state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json
$base = "http://127.0.0.1:$($state.port)"
$headers = @{ Authorization = "Bearer $($state.token)" }

$result = switch ($Command) {
    'plugins' { Invoke-RestMethod -Uri "$base/api/plugins" -Headers $headers }
    'actions' { Invoke-RestMethod -Uri "$base/api/actions" -Headers $headers }
    'action-commands' { Invoke-RestMethod -Uri "$base/api/actions/commands" -Headers $headers }
    'hardware' { Invoke-RestMethod -Uri "$base/api/hardware" -Headers $headers }
    'config' { Invoke-RestMethod -Uri "$base/api/config" -Headers $headers }
    'pad-execute' {
        if ([string]::IsNullOrWhiteSpace($Id)) {
            throw 'pad-execute requires -Id <saved-action-id>'
        }
        $body = @{ id = $Id } | ConvertTo-Json -Compress
        Invoke-RestMethod -Method Post -Uri "$base/api/pad/execute" -Headers $headers -ContentType 'application/json' -Body $body
    }
}

$result | ConvertTo-Json -Depth 12
