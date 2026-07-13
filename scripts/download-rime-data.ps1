param(
    [string] $RepoRoot
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if (!$RepoRoot) {
    $RepoRoot = Join-Path $scriptDir ".."
}
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path

. (Join-Path $scriptDir "windows-env.ps1")
Initialize-RimePocWindowsEnv

function Convert-ToMsysPath {
    param([string] $Path)

    $full = (Resolve-Path -LiteralPath $Path).Path
    if ($full -match "^([A-Za-z]):\\?(.*)$") {
        $drive = $matches[1].ToLowerInvariant()
        $rest = $matches[2].Replace("\", "/")
        if ($rest) {
            return "/$drive/$rest"
        }
        return "/$drive"
    }

    return $full.Replace("\", "/")
}

$msysRoot = if ($env:MSYS2_ROOT) { $env:MSYS2_ROOT } else { "C:\msys64" }
$bash = Join-Path $msysRoot "usr\bin\bash.exe"
if (!(Test-Path $bash)) {
    throw "MSYS2 bash was not found at $bash. Install MSYS2 or set MSYS2_ROOT."
}

$msysRepoRoot = Convert-ToMsysPath $RepoRoot
if ($msysRepoRoot.Contains("'")) {
    throw "Repository path contains a single quote, which this wrapper cannot safely pass to bash."
}

& $bash -lc "cd '$msysRepoRoot' && bash scripts/download-rime-data.sh"
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
