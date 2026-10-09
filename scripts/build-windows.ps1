# Build Minimal TTS for Windows: a per-user installer and a portable zip,
# both from one assembled folder.
#
# Bundles the release binary (plus a console copy for terminals), the fp32
# model + voices, espeak-ng (lib + data) and the VC++ runtime it needs, so
# either runs on a clean Windows 10/11 with nothing installed.
# Usage: scripts/build-windows.ps1     ($env:VERSION names the outputs, -> dist/)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = if ($env:VERSION) { $env:VERSION } else { 'dev' }
$name = "Minimal_TTS-$version-x86_64-windows"
$dist = Join-Path $root dist
$work = Join-Path ([IO.Path]::GetTempPath()) "mtts-$([guid]::NewGuid())"
$pkg = Join-Path $work $name

try {
    # prerequisites
    if (-not (Test-Path target\release\minimal-tts.exe)) {
        cargo build --release
        if ($LASTEXITCODE) { throw 'cargo build failed' }
    }
    if (-not (Test-Path models\kokoro.onnx)) { throw 'models\kokoro.onnx missing: run scripts/get-models.sh' }

    # espeak-ng 1.51 — the version the Ubuntu 24.04 AppImage build vendors, so
    # both platforms phonemize identically. An administrative install unpacks
    # the MSI without installing anything.
    if (-not (Test-Path vendor\libespeak-ng.dll)) {
        $msi = Join-Path $work 'espeak-ng-X64.msi'
        $x = Join-Path $work 'espeak'
        New-Item -ItemType Directory -Force $work, $x | Out-Null
        curl.exe -L -f --retry 3 -o $msi https://github.com/espeak-ng/espeak-ng/releases/download/1.51/espeak-ng-X64.msi
        if ($LASTEXITCODE) { throw 'espeak-ng download failed' }
        $p = Start-Process msiexec -ArgumentList "/a `"$msi`" /qn TARGETDIR=`"$x`"" -Wait -PassThru
        if ($p.ExitCode) { throw "msiexec /a failed ($($p.ExitCode))" }
        New-Item -ItemType Directory -Force vendor | Out-Null
        Copy-Item "$x\eSpeak NG\libespeak-ng.dll" vendor\
        Copy-Item -Recurse "$x\eSpeak NG\espeak-ng-data" vendor\
    }

    # VC++ runtime, app-local: both the exe and libespeak-ng.dll import it, and
    # a clean Windows doesn't have it. The loader searches the exe's directory
    # first, which also covers the DLL loaded from vendor\.
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $crt = Get-ChildItem "$vs\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" -Directory |
        Sort-Object FullName | Select-Object -Last 1
    if (-not $crt) { throw 'VC++ redist directory not found' }
    $runtime = 'msvcp140.dll', 'msvcp140_1.dll', 'vcruntime140.dll', 'vcruntime140_1.dll'

    # assemble with the exe-relative layout the app resolves
    New-Item -ItemType Directory -Force $pkg, "$pkg\vendor", "$pkg\models" | Out-Null
    Copy-Item target\release\minimal-tts.exe $pkg\
    # minimal-tts.exe is a GUI-subsystem binary, so shells neither wait for it
    # nor show its output. The CLI copy is the same bytes with the PE optional
    # header's Subsystem field (offset 0x5C from the PE signature, identical
    # for PE32+) set to 3 = console.
    $exe = [IO.File]::ReadAllBytes("$pkg\minimal-tts.exe")
    $pe = [BitConverter]::ToInt32($exe, 0x3C)
    if ($exe[$pe + 0x5C] -ne 2) { throw "unexpected subsystem $($exe[$pe + 0x5C]) (want 2 = GUI)" }
    $exe[$pe + 0x5C] = 3
    [IO.File]::WriteAllBytes("$pkg\minimal-tts-cli.exe", $exe)
    foreach ($dll in $runtime) { Copy-Item (Join-Path $crt.FullName $dll) $pkg\ }
    Copy-Item vendor\libespeak-ng.dll "$pkg\vendor\"
    Copy-Item -Recurse vendor\espeak-ng-data "$pkg\vendor\"
    Copy-Item models\kokoro.onnx, models\voices-v1.0.bin "$pkg\models\"
    Copy-Item LICENSE, README.md $pkg\

    New-Item -ItemType Directory -Force $dist | Out-Null
    $zip = Join-Path $dist "$name.zip"
    if (Test-Path $zip) { Remove-Item $zip }
    # bsdtar ships with Windows and is far faster than Compress-Archive
    tar.exe -a -c -f $zip -C $work $name
    if ($LASTEXITCODE) { throw 'zip failed' }
    Write-Host "built: $zip"

    # the installer, from the same folder; the icon is the one build.rs
    # rendered into the exe
    $iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
    if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
    if (-not (Test-Path $iscc)) { throw 'Inno Setup 6 (ISCC.exe) not found' }
    $ico = Get-ChildItem target\release\build\minimal-tts-*\out\minimal-tts.ico |
        Sort-Object LastWriteTime | Select-Object -Last 1
    & $iscc /Q "/DAppVersion=$($version.TrimStart('v'))" "/DSourceDir=$pkg" "/DIconFile=$($ico.FullName)" `
        "/DOutputDir=$dist" "/DOutputName=$name-setup" scripts\minimal-tts.iss
    if ($LASTEXITCODE) { throw 'ISCC failed' }
    Write-Host "built: $(Join-Path $dist "$name-setup.exe")"
}
finally {
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
}
