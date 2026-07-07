# Remove the Minimal TTS Windows shortcuts. Leaves the code and model weights alone.
$ErrorActionPreference = "Stop"

$links = @(
  Join-Path ([Environment]::GetFolderPath("Programs")) "Minimal TTS.lnk"
  Join-Path ([Environment]::GetFolderPath("Desktop"))  "Minimal TTS.lnk"
)
foreach ($lnk in $links) { Remove-Item -LiteralPath $lnk -ErrorAction SilentlyContinue }

Write-Host "Removed. (Run scripts\install.ps1 to reinstall.)"
