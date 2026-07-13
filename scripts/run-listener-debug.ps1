$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

if (!$env:PINYIN_NATIVE_LOG_EVENTS) {
    $env:PINYIN_NATIVE_LOG_EVENTS = "1"
}

$logDir = if ($env:PINYIN_LOG_DIR) {
    $env:PINYIN_LOG_DIR
} else {
    Join-Path $RepoRoot "logs"
}

New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logFile = if ($env:PINYIN_LOG_FILE) {
    $env:PINYIN_LOG_FILE
} else {
    Join-Path $logDir "pinyin-listener-$timestamp.log"
}

Write-Host "pinyin debug listener log:"
Write-Host "  $logFile"
Write-Host ""

& (Join-Path $PSScriptRoot "run-listener.ps1") --doctor --log-events @args 2>&1 |
    Tee-Object -FilePath $logFile -Append
exit $LASTEXITCODE
