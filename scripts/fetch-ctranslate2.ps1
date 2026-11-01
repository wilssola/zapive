# Fetches the official CTranslate2 build that Zapive links against on Windows.
#
#   .\scripts\fetch-ctranslate2.ps1
#
# Downloads the pinned PyPI wheel (sha256 verified), extracts ctranslate2.dll
# and libiomp5md.dll into vendor\ctranslate2-<version>\ and generates,
# next to them, the import library ctranslate2.lib plus two empty libraries
# (cpu_features.lib, cudart_static.lib) that ct2rs' `system` mode asks the linker
# for but that the DLL build does not need. build.rs links against this folder
# (through LIBRARY_PATH in .cargo/config.toml) and copies the DLLs next to the exe.
#
# Why the official build: ct2rs hard-codes the static C runtime (/MT) when it
# compiles CTranslate2 itself, which cannot be linked with the dynamic runtime
# (/MD) the rest of Zapive needs (Skia ships no prebuilt static-CRT binaries).
# The official build is a DLL with fast CPU kernels (oneDNN + Intel OpenMP) that
# loads cuBLAS only when a CUDA device is requested. ctranslate2 is MIT licensed.
#
# Needs the MSVC build tools (dumpbin.exe, lib.exe), i.e. what Rust's MSVC
# toolchain already requires.

$ErrorActionPreference = 'Stop'
$version = '4.8.4'
$wheel = "ctranslate2-$version-cp312-cp312-win_amd64.whl"
$url = 'https://files.pythonhosted.org/packages/af/25/cca7f6807d37cbfd9eebed8bbf136f5214c31d7d66de267ad05b676e9deb/' + $wheel
$sha256 = '056db4b4c2a31b7811d453155a3d6ab316ddb1dc104f413beee7bce26bcbf92d'

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root "vendor\ctranslate2-$version"
if ((Test-Path "$out\ctranslate2.lib") -and (Test-Path "$out\ctranslate2.dll") -and (Test-Path "$out\libiomp5md.dll")) {
    Write-Host "ctranslate2 $version already in $out"
    exit 0
}

# MSVC tools (x64) via vswhere.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$tools = & $vswhere -latest -products * -find 'VC\Tools\MSVC\*\bin\Hostx64\x64\lib.exe' | Select-Object -First 1
if (-not $tools) { throw 'MSVC build tools not found (lib.exe); install the Visual Studio C++ build tools' }
$bin = Split-Path -Parent $tools
$dumpbin = Join-Path $bin 'dumpbin.exe'
$lib = Join-Path $bin 'lib.exe'

$work = Join-Path $env:TEMP "zapive-ct2-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    $whl = Join-Path $work $wheel
    curl.exe -sSfL -o $whl $url
    if ($LASTEXITCODE) { throw "download failed: $url" }
    $got = (Get-FileHash $whl -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $sha256) { throw "sha256 mismatch for ${wheel}: $got" }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($whl)
    New-Item -ItemType Directory -Force $out | Out-Null
    try {
        foreach ($name in 'ctranslate2.dll', 'libiomp5md.dll') {
            $entry = $zip.Entries | Where-Object FullName -EQ "ctranslate2/$name"
            if (-not $entry) { throw "$name not in the wheel" }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $out $name), $true)
        }
    } finally { $zip.Dispose() }

    # Import library: every exported (C++ mangled) name of the DLL.
    $names = & $dumpbin /nologo /exports (Join-Path $out 'ctranslate2.dll') |
        ForEach-Object { if ($_ -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $Matches[1] } }
    if ($names.Count -lt 1000) { throw "unexpected export list ($($names.Count) names)" }
    $def = Join-Path $work 'ctranslate2.def'
    (@('LIBRARY ctranslate2.dll', 'EXPORTS') + $names) | Set-Content -Encoding ascii $def
    & $lib /nologo /machine:x64 "/def:$def" "/out:$out\ctranslate2.lib" | Out-Null
    if ($LASTEXITCODE) { throw 'lib.exe failed for ctranslate2.lib' }
    Remove-Item "$out\ctranslate2.exp" -ErrorAction SilentlyContinue

    # Empty libraries for the names ct2rs links unconditionally in `system` mode.
    foreach ($stub in 'cpu_features', 'cudart_static') {
        $sdef = Join-Path $work "$stub.def"
        @("LIBRARY $stub", 'EXPORTS') | Set-Content -Encoding ascii $sdef
        & $lib /nologo /machine:x64 "/def:$sdef" "/out:$out\$stub.lib" | Out-Null
        if ($LASTEXITCODE) { throw "lib.exe failed for $stub.lib" }
        Remove-Item "$out\$stub.exp" -ErrorAction SilentlyContinue
    }
    Write-Host "ctranslate2 $version -> $out ($($names.Count) exports)"
} finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
