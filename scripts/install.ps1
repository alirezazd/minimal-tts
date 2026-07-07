# Install Minimal TTS as a Windows app: Start Menu + Desktop shortcuts that start the
# server, open the app window, and shut the server down when you close it.
$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $PSScriptRoot
$uv   = (Get-Command uv -ErrorAction SilentlyContinue).Source
if (-not $uv) { throw "uv not found on PATH — install it first: https://docs.astral.sh/uv/" }

# Sync deps up front so the first launch is fast and offline.
Push-Location $here; & $uv sync --frozen; Pop-Location

# Run hidden (no console window) via powershell; uv opens the app window and owns its life.
$args = "-NoProfile -WindowStyle Hidden -Command `"& '$uv' run --directory '$here' main.py`""

$shell = New-Object -ComObject WScript.Shell
$links = @(
  Join-Path ([Environment]::GetFolderPath("Programs")) "Minimal TTS.lnk"
  Join-Path ([Environment]::GetFolderPath("Desktop"))  "Minimal TTS.lnk"
)
foreach ($lnk in $links) {
  $s = $shell.CreateShortcut($lnk)
  $s.TargetPath       = "powershell.exe"
  $s.Arguments        = $args
  $s.WorkingDirectory = $here
  $s.Description      = "Local read-aloud"
  # For a custom icon, drop a .ico in scripts\ and set: $s.IconLocation = "$here\scripts\minimal-tts.ico"
  $s.Save()
}

Write-Host "Installed. Launch 'Minimal TTS' from the Start Menu."
