# Install Minimal TTS as a Windows app: Start Menu + Desktop shortcuts that open the app window.
$ErrorActionPreference = "Stop"

$here   = Split-Path -Parent $PSScriptRoot
$launch = Join-Path $here "scripts\launch.ps1"

# Sync deps up front so the first launch is fast and offline.
Push-Location $here; uv sync --frozen; Pop-Location

$shell = New-Object -ComObject WScript.Shell
$links = @(
  Join-Path ([Environment]::GetFolderPath("Programs")) "Minimal TTS.lnk"
  Join-Path ([Environment]::GetFolderPath("Desktop"))  "Minimal TTS.lnk"
)
foreach ($lnk in $links) {
  $s = $shell.CreateShortcut($lnk)
  # Run launch.ps1 with no visible console window.
  $s.TargetPath       = "powershell.exe"
  $s.Arguments        = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$launch`""
  $s.WorkingDirectory = $here
  $s.Description      = "Local read-aloud"
  # For a custom icon, drop a .ico in scripts\ and set: $s.IconLocation = "$here\scripts\minimal-tts.ico"
  $s.Save()
}

Write-Host "Installed. Launch 'Minimal TTS' from the Start Menu."
