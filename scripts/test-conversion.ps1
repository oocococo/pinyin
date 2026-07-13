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

if ($args.Count -gt 0) {
    & cargo run --target x86_64-pc-windows-gnu -- @args
    exit $LASTEXITCODE
}

function Get-TomlString {
    param([string] $Key)

    $line = Get-Content pinyin.toml |
        Where-Object { $_ -match "^\s*$Key\s*=\s*`"(.*)`"\s*$" } |
        Select-Object -First 1
    if (!$line) {
        throw "unable to read $Key from pinyin.toml"
    }
    return ([regex]::Match($line, "^\s*$Key\s*=\s*`"(.*)`"\s*$")).Groups[1].Value
}

$triggerPrefix = if ($env:PINYIN_TEST_TRIGGER_PREFIX) {
    $env:PINYIN_TEST_TRIGGER_PREFIX
} else {
    Get-TomlString "trigger_prefix"
}
$triggerSuffix = if ($env:PINYIN_TEST_TRIGGER_SUFFIX) {
    $env:PINYIN_TEST_TRIGGER_SUFFIX
} else {
    Get-TomlString "trigger_suffix"
}

function New-TriggeredText {
    param([string] $Body)
    "$triggerPrefix$Body$triggerSuffix"
}

& cargo fmt --check
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

& cargo test --target x86_64-pc-windows-gnu
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

function Invoke-ConversionCase {
    param(
        [string] $InputText,
        [string] $Expected,
        [string[]] $ExtraArgs = @()
    )

    $cargoArgs = @("run", "--quiet", "--target", "x86_64-pc-windows-gnu", "--") + $ExtraArgs + @($InputText)
    $output = & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Error "conversion command failed for input: $InputText"
        exit $LASTEXITCODE
    }

    $actual = ($output | Select-String -Pattern '^output:\s*(.*)$' | Select-Object -First 1).Matches.Groups[1].Value
    if ($actual -ne $Expected) {
        Write-Error "case failed`ninput:    $InputText`nexpected: $Expected`nactual:   $actual`n$output"
        exit 1
    }

    Write-Host "ok: $InputText => $actual"
}

function From-Utf8Base64 {
    param([string] $Value)
    [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Value))
}

Invoke-ConversionCase (New-TriggeredText 'hao,zaijian,nihaoma,woyaoceshi') `
    (From-Utf8Base64 '5aW977yM5YaN6KeB77yM5L2g5aW95ZCX77yM5oiR6KaB5rWL6K+V')
Invoke-ConversionCase (New-TriggeredText 'woyaoceshizhongwenshurufa,nihaoma!hao...zaijian-jiahao+wenhao~') `
    (From-Utf8Base64 '5oiR6KaB5rWL6K+V5Lit5paH6L6T5YWl5rOV77yM5L2g5aW95ZCX77yB5aW94oCm4oCm5YaN6KeB77yN5Yqg5Y+377yL6Zeu5Y+3772e')
Invoke-ConversionCase (New-TriggeredText (From-Utf8Base64 'aGFv4oCm4oCmemFpamlhbixuaWhhb21h')) `
    (From-Utf8Base64 '5aW94oCm4oCm5YaN6KeB77yM5L2g5aW95ZCX')
Invoke-ConversionCase (New-TriggeredText 'wo ai OpenAI,yong Rust kaifa') `
    (From-Utf8Base64 '5oiR54ixT3BlbkFJ77yM55SoUnVzdOW8gOWPkQ==') `
    -ExtraArgs @("--conversion-mode", "rime-auto")

Write-Host "all conversion smoke tests passed"
