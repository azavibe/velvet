param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$TauriArgs
)

$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent $PSScriptRoot

function Find-Executable {
    param(
        [string]$ConfiguredPath,
        [string]$CommandName,
        [string[]]$Candidates
    )

    if ($ConfiguredPath -and (Test-Path -LiteralPath $ConfiguredPath -PathType Leaf)) {
        return (Resolve-Path -LiteralPath $ConfiguredPath).Path
    }

    $command = Get-Command $CommandName -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    foreach ($candidate in $Candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    return $null
}

$cmake = Find-Executable `
    -ConfiguredPath $env:CMAKE `
    -CommandName "cmake" `
    -Candidates @(
        "$env:ProgramFiles\CMake\bin\cmake.exe",
        "$env:LOCALAPPDATA\Programs\CMake\bin\cmake.exe"
    )
if (-not $cmake) {
    throw "CMake is required for local transcription. Install it with: winget install --id Kitware.CMake --exact"
}
$env:CMAKE = $cmake

$libclangCandidates = @()
if ($env:LIBCLANG_PATH) {
    $libclangCandidates += (Join-Path $env:LIBCLANG_PATH "libclang.dll")
    $libclangCandidates += $env:LIBCLANG_PATH
}
$libclangCandidates += "$env:ProgramFiles\LLVM\bin\libclang.dll"
$libclangCandidates += "$env:LOCALAPPDATA\Programs\LLVM\bin\libclang.dll"
$libclang = $libclangCandidates |
    Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } |
    Select-Object -First 1
if (-not $libclang) {
    throw "LLVM/libclang is required for local transcription. Install it with: winget install --id LLVM.LLVM --exact"
}
$env:LIBCLANG_PATH = Split-Path -Parent (Resolve-Path -LiteralPath $libclang).Path

$tauri = Join-Path $repositoryRoot "node_modules\.bin\tauri.exe"
if (-not (Test-Path -LiteralPath $tauri -PathType Leaf)) {
    throw "Tauri CLI is not installed. Run: bun install"
}

& $tauri @TauriArgs
exit $LASTEXITCODE
