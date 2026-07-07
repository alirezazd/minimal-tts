# Open Minimal TTS as a chromeless Chrome app window, starting the backend if needed.
$ErrorActionPreference = "Stop"

$port = if ($env:MINIMAL_TTS_PORT) { [int]$env:MINIMAL_TTS_PORT } else { 8765 }
$url  = "http://127.0.0.1:$port"
$here = Split-Path -Parent $PSScriptRoot          # project root (scripts\..)
$profileDir = Join-Path $here ".chrome-profile"

function Test-Port {
  try { $c = [Net.Sockets.TcpClient]::new(); $c.Connect("127.0.0.1", $port); $c.Close(); $true }
  catch { $false }
}

# Bring up the backend if it isn't already listening.
if (-not (Test-Port)) {
  $env:MINIMAL_TTS_NO_BROWSER = "1"
  Start-Process -WindowStyle Hidden -WorkingDirectory $here -FilePath "uv" -ArgumentList "run", "main.py"
}
for ($i = 0; $i -lt 100 -and -not (Test-Port); $i++) { Start-Sleep -Milliseconds 200 }

$chrome = @(
  "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
  "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
  "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe",
  "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $chrome) { Start-Process $url; return }   # no Chrome/Edge — default browser

Start-Process -FilePath $chrome -ArgumentList `
  "--app=$url", "--user-data-dir=$profileDir", "--no-first-run", "--no-default-browser-check"
