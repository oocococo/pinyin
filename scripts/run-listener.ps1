$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot
. (Join-Path $PSScriptRoot "windows-env.ps1")
Initialize-PinyinWindowsEnv

if (!$env:RIME_SHARED_DATA_DIR) {
    $env:RIME_SHARED_DATA_DIR = Join-Path $RepoRoot "data\shared"
}

if (!$env:RIME_USER_DATA_DIR) {
    $env:RIME_USER_DATA_DIR = Join-Path $RepoRoot "data\user"
}

if (!$env:RIME_SCHEMA) {
    $env:RIME_SCHEMA = "luna_pinyin_simp"
}

if (!(Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo was not found. Install Rust for Windows, then open a new PowerShell window."
    exit 1
}

function Ensure-RustTarget {
    param([string] $Target)

    if (!(Get-Command rustup -ErrorAction SilentlyContinue)) {
        return
    }

    $installedTargets = & rustup target list --installed 2>$null
    if ($LASTEXITCODE -eq 0 -and ($installedTargets -contains $Target)) {
        return
    }

    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & rustup target add $Target
    $targetExitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousPreference

    if ($targetExitCode -ne 0) {
        exit $targetExitCode
    }
}

if (Get-Command rustup -ErrorAction SilentlyContinue) {
    Ensure-RustTarget "x86_64-pc-windows-gnu"
}

if (!$env:RIME_INCLUDE_DIR -or !(Test-Path (Join-Path $env:RIME_INCLUDE_DIR "rime_api.h"))) {
    Write-Error "rime_api.h was not found. Set RIME_INCLUDE_DIR or run scripts\check-librime.ps1."
    exit 1
}

if (
    !$env:RIME_LIB_DIR -or
    !(
        (Test-Path (Join-Path $env:RIME_LIB_DIR "rime.lib")) -or
        (Test-Path (Join-Path $env:RIME_LIB_DIR "librime.lib")) -or
        (Test-Path (Join-Path $env:RIME_LIB_DIR "rime.dll.a")) -or
        (Test-Path (Join-Path $env:RIME_LIB_DIR "librime.dll.a"))
    )
) {
    Write-Error "librime import library was not found. Set RIME_LIB_DIR or run scripts\check-librime.ps1."
    exit 1
}

if (!(Test-Path $env:RIME_SHARED_DATA_DIR)) {
    Write-Error "Rime shared data dir does not exist: $env:RIME_SHARED_DATA_DIR"
    Write-Error "Run bash scripts/download-rime-data.sh or set RIME_SHARED_DATA_DIR to an existing Rime data folder."
    exit 1
}

& cmd.exe /d /c "cargo build --target x86_64-pc-windows-gnu 2>&1"
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$exe = Join-Path $RepoRoot "target\x86_64-pc-windows-gnu\debug\pinyin.exe"
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& $exe --listen @args 2>&1 | ForEach-Object { Write-Output ($_.ToString()) }
$listenerExitCode = $LASTEXITCODE
$ErrorActionPreference = $previousPreference
exit $listenerExitCode
