$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

if (!$env:RIME_POC_NATIVE_LOG_EVENTS) {
    $env:RIME_POC_NATIVE_LOG_EVENTS = "1"
}

$logDir = if ($env:RIME_POC_LOG_DIR) {
    $env:RIME_POC_LOG_DIR
} else {
    Join-Path $RepoRoot "logs"
}

New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logFile = if ($env:RIME_POC_LOG_FILE) {
    $env:RIME_POC_LOG_FILE
} else {
    Join-Path $logDir "rime-poc-listener-$timestamp.log"
}

Write-Host "rime-poc debug listener log:"
Write-Host "  $logFile"
Write-Host ""

& (Join-Path $PSScriptRoot "run-listener.ps1") --doctor --log-events @args 2>&1 |
    Tee-Object -FilePath $logFile -Append
exit $LASTEXITCODE
