function Initialize-RimePocWindowsEnv {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if ((Test-Path $cargoBin) -and (($env:PATH -split ';') -notcontains $cargoBin)) {
        $env:PATH = "$cargoBin;$env:PATH"
    }

    $msysRoot = if ($env:MSYS2_ROOT) { $env:MSYS2_ROOT } else { "C:\msys64" }
    $mingwRoot = Join-Path $msysRoot "mingw64"
    $mingwBin = Join-Path $mingwRoot "bin"

    if (Test-Path $mingwBin) {
        if (($env:PATH -split ';') -notcontains $mingwBin) {
            $env:PATH = "$mingwBin;$env:PATH"
        }

        if (!$env:RIME_INCLUDE_DIR) {
            $env:RIME_INCLUDE_DIR = Join-Path $mingwRoot "include"
        }

        if (!$env:RIME_LIB_DIR) {
            $env:RIME_LIB_DIR = Join-Path $mingwRoot "lib"
        }

        $gcc = Join-Path $mingwBin "gcc.exe"
        $gxx = Join-Path $mingwBin "g++.exe"
        if ((Test-Path $gcc) -and !$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER) {
            $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = $gcc
        }
        if ((Test-Path $gcc) -and !${env:CC_x86_64_pc_windows_gnu}) {
            ${env:CC_x86_64_pc_windows_gnu} = $gcc
        }
        if ((Test-Path $gxx) -and !${env:CXX_x86_64_pc_windows_gnu}) {
            ${env:CXX_x86_64_pc_windows_gnu} = $gxx
        }
    }

    $llvmBin = "C:\Program Files\LLVM\bin"
    if ((Test-Path (Join-Path $llvmBin "libclang.dll"))) {
        if (!$env:LIBCLANG_PATH) {
            $env:LIBCLANG_PATH = $llvmBin
        }
        if (($env:PATH -split ';') -notcontains $llvmBin) {
            $env:PATH = "$llvmBin;$env:PATH"
        }
    }
}
