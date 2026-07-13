$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot
. (Join-Path $PSScriptRoot "windows-env.ps1")
Initialize-RimePocWindowsEnv

function Find-FirstExistingPath {
    param(
        [string[]] $Candidates,
        [string] $RequiredChild
    )

    foreach ($candidate in $Candidates) {
        if ([string]::IsNullOrWhiteSpace($candidate)) {
            continue
        }

        $path = [Environment]::ExpandEnvironmentVariables($candidate)
        if (Test-Path (Join-Path $path $RequiredChild)) {
            return (Resolve-Path $path).Path
        }
    }

    return $null
}

$includeCandidates = @(
    $env:RIME_INCLUDE_DIR,
    "C:\msys64\mingw64\include",
    (Join-Path $RepoRoot "librime\include"),
    (Join-Path $RepoRoot "vendor\librime\include"),
    "C:\librime\include",
    "C:\Program Files\librime\include"
)

$libCandidates = @(
    $env:RIME_LIB_DIR,
    "C:\msys64\mingw64\lib",
    (Join-Path $RepoRoot "librime\lib"),
    (Join-Path $RepoRoot "vendor\librime\lib"),
    "C:\librime\lib",
    "C:\Program Files\librime\lib"
)

$includeDir = if ($env:RIME_INCLUDE_DIR) {
    $env:RIME_INCLUDE_DIR
} else {
    Find-FirstExistingPath $includeCandidates "rime_api.h"
}

$libDir = if ($env:RIME_LIB_DIR) {
    $env:RIME_LIB_DIR
} else {
    $found = $null
    foreach ($candidate in $libCandidates) {
        if ([string]::IsNullOrWhiteSpace($candidate)) {
            continue
        }
        $path = [Environment]::ExpandEnvironmentVariables($candidate)
        if (
            (Test-Path (Join-Path $path "rime.lib")) -or
            (Test-Path (Join-Path $path "librime.lib")) -or
            (Test-Path (Join-Path $path "rime.dll.a")) -or
            (Test-Path (Join-Path $path "librime.dll.a"))
        ) {
            $found = (Resolve-Path $path).Path
            break
        }
    }
    $found
}

Write-Host "rime-poc Windows librime check"
Write-Host "include dir: $includeDir"
Write-Host "lib dir:     $libDir"

$missing = $false

if (!$includeDir -or !(Test-Path (Join-Path $includeDir "rime_api.h"))) {
    Write-Host "missing: rime_api.h. Set RIME_INCLUDE_DIR to the folder containing rime_api.h." -ForegroundColor Red
    $missing = $true
}

if (
    !$libDir -or
    !(
        (Test-Path (Join-Path $libDir "rime.lib")) -or
        (Test-Path (Join-Path $libDir "librime.lib")) -or
        (Test-Path (Join-Path $libDir "rime.dll.a")) -or
        (Test-Path (Join-Path $libDir "librime.dll.a"))
    )
) {
    Write-Host "missing: librime import library. Set RIME_LIB_DIR to the folder containing rime.lib/librime.lib or rime.dll.a/librime.dll.a." -ForegroundColor Red
    $missing = $true
}

if ($missing) {
    exit 1
}

Write-Host "found:       rime_api.h"
Write-Host "found:       librime library"
Write-Host ""
Write-Host "Use these values in the current PowerShell session:"
Write-Host "`$env:RIME_INCLUDE_DIR = `"$includeDir`""
Write-Host "`$env:RIME_LIB_DIR = `"$libDir`""
