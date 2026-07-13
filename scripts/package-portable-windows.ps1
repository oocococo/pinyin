$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

. (Join-Path $PSScriptRoot "windows-env.ps1")
Initialize-PinyinWindowsEnv

if (!(Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo was not found. Install Rust for Windows, then open a new PowerShell window."
    exit 1
}

$msysRoot = if ($env:MSYS2_ROOT) { $env:MSYS2_ROOT } else { "C:\msys64" }
$mingwRoot = Join-Path $msysRoot "mingw64"
$mingwBin = Join-Path $mingwRoot "bin"
$objdump = Join-Path $mingwBin "objdump.exe"

if (!(Test-Path $objdump)) {
    Write-Error "objdump.exe was not found at $objdump. Install mingw-w64-x86_64-binutils through MSYS2."
    exit 1
}

if (!$env:RIME_INCLUDE_DIR -or !(Test-Path (Join-Path $env:RIME_INCLUDE_DIR "rime_api.h"))) {
    Write-Error "rime_api.h was not found. Set RIME_INCLUDE_DIR or run scripts\check-librime.ps1."
    exit 1
}

if (!(Test-Path "data\shared")) {
    Write-Error "data\shared does not exist. Run scripts\download-rime-data.ps1 first."
    exit 1
}

if (Get-Command rustup -ErrorAction SilentlyContinue) {
    $installedTargets = & rustup target list --installed 2>$null
    if ($LASTEXITCODE -eq 0 -and ($installedTargets -notcontains "x86_64-pc-windows-gnu")) {
        & rustup target add x86_64-pc-windows-gnu
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }
}

& cmd.exe /d /c "cargo build --release --target x86_64-pc-windows-gnu 2>&1"
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$portableDir = Join-Path $RepoRoot "dist\pinyin-windows-portable"
$zipPath = Join-Path $RepoRoot "dist\pinyin-windows-portable.zip"
$dllDir = Join-Path $portableDir "bin"
$dataDir = Join-Path $portableDir "data"
$exeSource = Join-Path $RepoRoot "target\x86_64-pc-windows-gnu\release\pinyin.exe"
$exeDest = Join-Path $portableDir "pinyin.exe"

Remove-Item -LiteralPath $portableDir -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dllDir, (Join-Path $dataDir "shared"), (Join-Path $dataDir "user") | Out-Null

Copy-Item -LiteralPath $exeSource -Destination $exeDest
Copy-Item -LiteralPath "pinyin.toml" -Destination (Join-Path $portableDir "pinyin.toml")
Copy-Item -Path "data\shared\*" -Destination (Join-Path $dataDir "shared") -Recurse
if (Test-Path "data\user\default.custom.yaml") {
    Copy-Item -LiteralPath "data\user\default.custom.yaml" -Destination (Join-Path $dataDir "user\default.custom.yaml")
}

$openccSource = Join-Path $mingwRoot "share\opencc"
if (Test-Path $openccSource) {
    $openccDest = Join-Path $dataDir "shared\opencc"
    New-Item -ItemType Directory -Force -Path $openccDest | Out-Null
    Copy-Item -Path (Join-Path $openccSource "*") -Destination $openccDest -Recurse
}

$systemDlls = @(
    "advapi32.dll", "bcrypt.dll", "comctl32.dll", "comdlg32.dll", "crypt32.dll",
    "gdi32.dll", "imm32.dll", "kernel32.dll", "msvcrt.dll", "ntdll.dll",
    "ole32.dll", "oleaut32.dll", "rpcrt4.dll", "sechost.dll", "shell32.dll",
    "shlwapi.dll", "user32.dll", "userenv.dll", "version.dll", "winmm.dll",
    "ws2_32.dll"
)

function Get-ImportedDllNames {
    param([string] $Path)

    & $objdump -p $Path 2>$null |
        Select-String -Pattern 'DLL Name:\s*(.+)$' |
        ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() }
}

function Find-MingwDll {
    param([string] $Name)

    Get-ChildItem -LiteralPath $mingwBin -Filter $Name -File -ErrorAction SilentlyContinue |
        Select-Object -First 1
}

$queue = New-Object System.Collections.Generic.Queue[string]
$seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
$copied = New-Object System.Collections.Generic.List[string]
$queue.Enqueue($exeDest)

while ($queue.Count -gt 0) {
    $image = $queue.Dequeue()
    foreach ($dllName in Get-ImportedDllNames $image) {
        $lowerDllName = $dllName.ToLowerInvariant()
        if (
            ($systemDlls -contains $lowerDllName) -or
            $lowerDllName.StartsWith("api-ms-win-") -or
            $lowerDllName.StartsWith("ext-ms-win-")
        ) {
            continue
        }

        if (!$seen.Add($dllName)) {
            continue
        }

        $source = Find-MingwDll $dllName
        if (!$source) {
            $systemPath = Join-Path $env:WINDIR "System32\$dllName"
            if (Test-Path $systemPath) {
                continue
            }
            Write-Error "Imported DLL was not found under $mingwBin`: $dllName"
            exit 1
        }

        $dest = Join-Path $dllDir $source.Name
        Copy-Item -LiteralPath $source.FullName -Destination $dest
        $copied.Add($source.Name) | Out-Null
        $queue.Enqueue($dest)
    }
}

@'
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $ArgsForListener
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:PATH = (Join-Path $root "bin") + ";" + $env:PATH
$env:RIME_SHARED_DATA_DIR = if ($env:RIME_SHARED_DATA_DIR) { $env:RIME_SHARED_DATA_DIR } else { Join-Path $root "data\shared" }
$env:RIME_USER_DATA_DIR = if ($env:RIME_USER_DATA_DIR) { $env:RIME_USER_DATA_DIR } else { Join-Path $root "data\user" }
$env:RIME_SCHEMA = if ($env:RIME_SCHEMA) { $env:RIME_SCHEMA } else { "luna_pinyin_simp" }

& (Join-Path $root "pinyin.exe") --listen @ArgsForListener
exit $LASTEXITCODE
'@ | Set-Content -LiteralPath (Join-Path $portableDir "run-listener.ps1") -Encoding ASCII

@'
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $ArgsForListener
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:PATH = (Join-Path $root "bin") + ";" + $env:PATH
$env:RIME_SHARED_DATA_DIR = if ($env:RIME_SHARED_DATA_DIR) { $env:RIME_SHARED_DATA_DIR } else { Join-Path $root "data\shared" }
$env:RIME_USER_DATA_DIR = if ($env:RIME_USER_DATA_DIR) { $env:RIME_USER_DATA_DIR } else { Join-Path $root "data\user" }
$env:RIME_SCHEMA = if ($env:RIME_SCHEMA) { $env:RIME_SCHEMA } else { "luna_pinyin_simp" }
$env:PINYIN_NATIVE_LOG_EVENTS = if ($env:PINYIN_NATIVE_LOG_EVENTS) { $env:PINYIN_NATIVE_LOG_EVENTS } else { "1" }

$logDir = if ($env:PINYIN_LOG_DIR) { $env:PINYIN_LOG_DIR } else { Join-Path $root "logs" }
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$logFile = if ($env:PINYIN_LOG_FILE) {
    $env:PINYIN_LOG_FILE
} else {
    Join-Path $logDir ("pinyin-listener-" + (Get-Date -Format "yyyyMMdd-HHmmss") + ".log")
}

Write-Host "pinyin debug listener log:"
Write-Host "  $logFile"
Write-Host ""

$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& (Join-Path $root "pinyin.exe") --doctor --listen --log-events @ArgsForListener 2>&1 |
    ForEach-Object { $_.ToString() } |
    Tee-Object -FilePath $logFile -Append
$listenerExitCode = $LASTEXITCODE
$ErrorActionPreference = $previousPreference
exit $listenerExitCode
'@ | Set-Content -LiteralPath (Join-Path $portableDir "run-listener-debug.ps1") -Encoding ASCII

@'
pinyin portable Windows package

This directory bundles:
  - pinyin.exe
  - MinGW/librime DLL dependencies under bin\
  - Rime shared/user data under data\
  - pinyin.toml trigger and conversion config

No MSYS2 or librime install is required on the target Windows machine.

1. Start the listener:
   powershell -ExecutionPolicy Bypass -File .\run-listener.ps1

   For diagnosis with event logs:
   powershell -ExecutionPolicy Bypass -File .\run-listener-debug.ps1

2. Type a trigger in any normal text field:
   ''woyaoceshizhongwenshurufa,nihaoma!''

Expected output:
   我要测试中文输入法，你好吗！

Experimental mixed Chinese/English mode:
   powershell -ExecutionPolicy Bypass -File .\run-listener.ps1 --conversion-mode rime-auto

Or edit pinyin.toml:
   conversion_mode = "rime-auto"

Notes:
  - Windows does not need macOS-style Accessibility/Input Monitoring permission.
  - Injected input may not reach elevated apps unless pinyin.exe also runs elevated.
'@ | Set-Content -LiteralPath (Join-Path $portableDir "README.txt") -Encoding UTF8

$oldPath = $env:PATH
$env:PATH = "$dllDir;$env:PATH"
try {
    & $exeDest --body "woyaoceshi,nihaoma!" | Out-Null
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    & $exeDest --body --conversion-mode rime-auto "wo ai OpenAI,yong Rust kaifa" | Out-Null
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
finally {
    $env:PATH = $oldPath
}

Compress-Archive -LiteralPath $portableDir -DestinationPath $zipPath -Force

Write-Host "Portable package:"
Write-Host "  $portableDir"
Write-Host ""
Write-Host "Portable zip:"
Write-Host "  $zipPath"
Write-Host ""
Write-Host "Bundled DLLs:"
$copied | Sort-Object | ForEach-Object { Write-Host "  $_" }
